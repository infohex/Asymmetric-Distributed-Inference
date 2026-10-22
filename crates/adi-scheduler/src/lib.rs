//! Scheduler service. Every scheduling decision is made by
//! `adi_core::Scheduler`; this crate adds the clock, the HTTP surface, the
//! long-poll wake-up, the blob store and the expansion of an Event into U.

pub mod api;
pub mod config;
pub mod store;

use adi_core::metrics::{EventRecord, Metrics, RoundRecord};
use adi_core::scheduler::{Observation, RoundKind, Scheduler};
use adi_core::{NodeId, UnitId};
use api::*;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use config::Config;
use rand::RngCore;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use store::{hash_of, matrix_dims, Store};
use tokio::sync::{Mutex, Notify};

pub const LOGIC_MATMUL_BLOCK_F32_STRICT: &str = "matmul-block-f32-strict-v0";

struct NodeInfo {
    name: String,
    token: String,
}

struct EventState {
    et: usize,
    a: String,
    b: String,
    block: usize,
    units: Vec<UnitId>,
    done: usize,
    results: HashMap<UnitId, String>,
    submitted_at: f64,
    completed_at: Option<f64>,
    first_starve_at: Option<f64>,
    reclaims: u32,
    last_reclaim: Option<(f64, f64, f64)>,
    done_cost: f64,
}

struct UnitMeta {
    event: usize,
    bi: usize,
    bj: usize,
}

struct RoundInfo {
    node: NodeId,
    issued_at: f64,
    units: Vec<UnitId>,
    cost: f64,
    kind: RoundKind,
    r_est: f64,
}

/// Everything behind the lock.
pub struct Service {
    pub sched: Scheduler,
    nodes: Vec<NodeInfo>,
    by_name: HashMap<String, NodeId>,
    parked: HashSet<NodeId>,
    events: Vec<EventState>,
    unit_meta: Vec<UnitMeta>,
    rounds: HashMap<u64, RoundInfo>,
    next_round: u64,
    metrics: Metrics,
    w: f64,
}

pub struct AppState {
    pub svc: Mutex<Service>,
    pub notify: Mutex<Vec<Arc<Notify>>>,
    pub store: Store,
    pub start: Instant,
    pub config: Config,
}

pub type Shared = Arc<AppState>;

impl AppState {
    pub fn new(config: Config) -> std::io::Result<Shared> {
        let store = Store::new(config.data_dir.clone())?;
        let sched = Scheduler::new(config.scheduler.clone(), &[], &config.ets);
        let mut metrics = Metrics::default();
        metrics.timeout_mult = config.scheduler.timeout_mult;
        let svc = Service {
            sched,
            nodes: Vec::new(),
            by_name: HashMap::new(),
            parked: HashSet::new(),
            events: Vec::new(),
            unit_meta: Vec::new(),
            rounds: HashMap::new(),
            next_round: 1,
            metrics,
            w: config.scheduler.w,
        };
        Ok(Arc::new(AppState {
            svc: Mutex::new(svc),
            notify: Mutex::new(Vec::new()),
            store,
            start: Instant::now(),
            config,
        }))
    }

    pub fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }
}

impl Service {
    fn wake_candidates(&mut self) -> Vec<NodeId> {
        let mut out = Vec::new();
        let n_et = self.sched.ets().len();
        for et in 0..n_et {
            if self.sched.ets()[et].ready_cost() <= 0.0 {
                continue;
            }
            let best = self
                .parked
                .iter()
                .copied()
                .filter(|&n| self.sched.is_eligible(n, et))
                .max_by(|&a, &b| self.sched.estimate(a).r.total_cmp(&self.sched.estimate(b).r));
            if let Some(n) = best {
                self.parked.remove(&n);
                out.push(n);
            }
        }
        out
    }

    fn note_reclaim(&mut self, units: &[UnitId], now: f64) {
        self.metrics.reissued_units += units.len() as u64;
        let sum_r: f64 = (0..self.sched.node_count()).map(|n| self.sched.estimate(n).r).sum();
        let mut touched: Vec<usize> = units.iter().map(|&u| self.unit_meta[u].event).collect();
        touched.sort_unstable();
        touched.dedup();
        for e in touched {
            let ev = &mut self.events[e];
            let remaining = (ev.units.len() as f64 - ev.done_cost).max(0.0);
            ev.last_reclaim = Some((now, remaining, sum_r));
            ev.reclaims += 1;
        }
    }

    fn mark_starvation(&mut self, node: NodeId, now: f64) {
        for ev in self.events.iter_mut() {
            if ev.completed_at.is_none() && ev.first_starve_at.is_none() && self.sched.is_eligible(node, ev.et) {
                ev.first_starve_at = Some(now);
            }
        }
    }

    fn node_truth(&self) -> Vec<(String, f64, f64)> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.name.clone(), f64::NAN, self.sched.estimate(i).r))
            .collect()
    }

    fn finish_metrics(&mut self, now: f64) {
        self.metrics.sim_time = now;
        let mut busy: Vec<(String, f64)> = self.nodes.iter().map(|n| (n.name.clone(), 0.0)).collect();
        for r in &self.metrics.rounds {
            if let Some(b) = busy.iter_mut().find(|(n, _)| *n == r.node) {
                b.1 += r.returned_at - r.issued_at;
            }
        }
        self.metrics.busy = busy;
        self.metrics.transitions = (0..self.sched.node_count()).map(|n| self.sched.transitions(n)).sum();
        self.metrics.duplicate_results = self.sched.duplicate_results;
        self.metrics.late_results = self.sched.late_results;
        self.metrics.waiting_at_end = self.sched.waiting_units();
        self.metrics.churn = self.metrics.reissued_units > 0;
        self.metrics.total_units = self.unit_meta.len() as u64;
        self.metrics.total_cost = self.unit_meta.len() as f64;
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers.get("authorization")?.to_str().ok()?.strip_prefix("Bearer ")
}

fn new_token() -> String {
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn err(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

async fn check_node(svc: &Service, headers: &HeaderMap, node: usize) -> Result<(), Response> {
    let info = svc.nodes.get(node).ok_or_else(|| err(StatusCode::NOT_FOUND, "unknown node"))?;
    match bearer(headers) {
        Some(t) if t == info.token => Ok(()),
        _ => Err(err(StatusCode::UNAUTHORIZED, "bad node token")),
    }
}

fn check_admin(state: &AppState, headers: &HeaderMap) -> Result<(), Response> {
    if state.config.admin_token.is_empty() {
        return Ok(());
    }
    match bearer(headers) {
        Some(t) if t == state.config.admin_token => Ok(()),
        _ => Err(err(StatusCode::UNAUTHORIZED, "bad admin token")),
    }
}

async fn wake(state: &AppState, ids: Vec<NodeId>) {
    if ids.is_empty() {
        return;
    }
    let notify = state.notify.lock().await;
    for n in ids {
        if let Some(nt) = notify.get(n) {
            nt.notify_one();
        }
    }
}

// ---------------------------------------------------------------- handlers

async fn register(State(state): State<Shared>, Json(req): Json<RegisterReq>) -> Response {
    if req.c == 0 {
        return err(StatusCode::BAD_REQUEST, "c must be >= 1");
    }
    let now = state.now();
    let mut svc = state.svc.lock().await;
    let token = new_token();
    let (id, reclaimed) = match svc.by_name.get(&req.node_id).copied() {
        Some(id) => {
            svc.nodes[id].token = token.clone();
            let r = svc.sched.node_reconnected(id);
            (id, r)
        }
        None => {
            let id = svc.sched.add_node(req.c, &req.tier);
            svc.nodes.push(NodeInfo { name: req.node_id.clone(), token: token.clone() });
            svc.by_name.insert(req.node_id.clone(), id);
            state.notify.lock().await.push(Arc::new(Notify::new()));
            (id, None)
        }
    };
    let mut to_wake = Vec::new();
    if let Some(r) = reclaimed {
        svc.note_reclaim(&r.units, now);
        to_wake = svc.wake_candidates();
    }
    let resp = RegisterResp {
        node: id,
        token,
        w: svc.w,
        retry_ms: (svc.sched.retry_delay() * 1000.0) as u64,
    };
    drop(svc);
    wake(&state, to_wake).await;
    Json(resp).into_response()
}

async fn pull(State(state): State<Shared>, headers: HeaderMap, Json(req): Json<PullReq>) -> Response {
    let deadline = Instant::now() + std::time::Duration::from_millis(req.wait_ms.min(60_000));
    let mut attempts = 0;
    loop {
        attempts += 1;
        let now = state.now();
        let mut svc = state.svc.lock().await;
        if let Err(r) = check_node(&svc, &headers, req.node).await {
            return r;
        }
        if svc.sched.has_in_flight(req.node) {
            return err(StatusCode::CONFLICT, "a round is in flight: report it or re-register");
        }
        svc.parked.remove(&req.node);
        svc.metrics.pulls += 1;
        let round = svc.sched.pull(req.node, now);
        if !round.units.is_empty() {
            let round_id = svc.next_round;
            svc.next_round += 1;
            let et_name = svc.sched.ets()[round.et].name.clone();
            let units: Vec<UnitWire> = round
                .units
                .iter()
                .map(|&u| {
                    let m = &svc.unit_meta[u];
                    let ev = &svc.events[m.event];
                    UnitWire {
                        u_id: u,
                        e_id: m.event,
                        logic_hash: LOGIC_MATMUL_BLOCK_F32_STRICT.to_string(),
                        input: UnitInput { a: ev.a.clone(), b: ev.b.clone(), bi: m.bi, bj: m.bj, block: ev.block },
                    }
                })
                .collect();
            svc.rounds.insert(
                round_id,
                RoundInfo { node: req.node, issued_at: now, units: round.units.clone(), cost: round.cost, kind: round.kind, r_est: round.r_est },
            );
            return Json(PullResp { round_id, et: et_name, issued_at: now, units }).into_response();
        }
        svc.metrics.empty_pulls += 1;
        svc.mark_starvation(req.node, now);
        svc.parked.insert(req.node);
        drop(svc);
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || attempts > 4 {
            return StatusCode::NO_CONTENT.into_response();
        }
        let nt = state.notify.lock().await.get(req.node).cloned();
        match nt {
            Some(nt) => {
                let _ = tokio::time::timeout(remaining, nt.notified()).await;
            }
            None => return StatusCode::NO_CONTENT.into_response(),
        }
    }
}

async fn report(State(state): State<Shared>, headers: HeaderMap, Json(req): Json<ReportReq>) -> Response {
    let now = state.now();
    let mut svc = state.svc.lock().await;
    if let Err(r) = check_node(&svc, &headers, req.node).await {
        return r;
    }
    let info = match svc.rounds.remove(&req.round_id) {
        Some(i) if i.node == req.node => i,
        Some(i) => {
            svc.rounds.insert(req.round_id, i);
            return err(StatusCode::FORBIDDEN, "round belongs to another node");
        }
        None => return err(StatusCode::NOT_FOUND, "unknown round"),
    };
    // Only units whose result blob is present count.
    let mut returned: Vec<UnitId> = Vec::new();
    let mut hashes: HashMap<UnitId, String> = HashMap::new();
    for r in &req.results {
        if info.units.contains(&r.u_id) && state.store.has(&r.hash) {
            returned.push(r.u_id);
            hashes.insert(r.u_id, r.hash.clone());
        }
    }
    let obs = Observation { node: req.node, issued_at: info.issued_at, returned_at: now, cost: info.cost, bytes: req.bytes_in };
    let accepted = svc.sched.round_done(obs, &returned);
    let node_name = svc.nodes[req.node].name.clone();
    svc.metrics.rounds.push(RoundRecord {
        node: node_name,
        issued_at: info.issued_at,
        returned_at: now,
        units: info.units.len(),
        cost: info.cost,
        bytes: req.bytes_in,
        kind: info.kind,
        r_est: info.r_est,
        r_true_eff: f64::NAN,
    });
    for &u in &accepted {
        let e = svc.unit_meta[u].event;
        let h = hashes.get(&u).cloned().unwrap_or_default();
        let ev = &mut svc.events[e];
        ev.results.insert(u, h);
        ev.done += 1;
        ev.done_cost += 1.0;
        if ev.done == ev.units.len() && ev.completed_at.is_none() {
            ev.completed_at = Some(now);
            let rec = EventRecord {
                event: e,
                submitted_at: ev.submitted_at,
                completed_at: now,
                first_starve_at: ev.first_starve_at,
                reclaims: ev.reclaims,
                last_reclaim: ev.last_reclaim,
            };
            svc.metrics.events.push(rec);
        }
    }
    let to_wake = svc.wake_candidates();
    drop(svc);
    wake(&state, to_wake).await;
    Json(ReportResp { accepted }).into_response()
}

async fn blob_put(State(state): State<Shared>, headers: HeaderMap, Path(hash): Path<String>, body: Bytes) -> Response {
    if bearer(&headers).is_none() && !state.config.admin_token.is_empty() {
        return err(StatusCode::UNAUTHORIZED, "token required");
    }
    if hash_of(&body) != hash {
        return err(StatusCode::BAD_REQUEST, "hash does not match body");
    }
    match state.store.put(body) {
        Ok(_) => StatusCode::CREATED.into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn blob_get(State(state): State<Shared>, Path(hash): Path<String>) -> Response {
    match state.store.get(&hash) {
        Some(b) => b.into_response(),
        None => err(StatusCode::NOT_FOUND, "no such blob"),
    }
}

async fn submit(State(state): State<Shared>, headers: HeaderMap, Json(req): Json<SubmitReq>) -> Response {
    if let Err(r) = check_admin(&state, &headers) {
        return r;
    }
    if req.block == 0 {
        return err(StatusCode::BAD_REQUEST, "block must be >= 1");
    }
    let a = match state.store.get(&req.a_hash).and_then(|b| matrix_dims(&b)) {
        Some(d) => d,
        None => return err(StatusCode::BAD_REQUEST, "a_hash: no such matrix blob"),
    };
    let b = match state.store.get(&req.b_hash).and_then(|b| matrix_dims(&b)) {
        Some(d) => d,
        None => return err(StatusCode::BAD_REQUEST, "b_hash: no such matrix blob"),
    };
    if a.1 != b.0 {
        return err(StatusCode::BAD_REQUEST, format!("shape mismatch: A is {}x{}, B is {}x{}", a.0, a.1, b.0, b.1));
    }
    let (m, k, n) = (a.0, a.1, b.1);
    let now = state.now();
    let mut svc = state.svc.lock().await;
    let et = match &req.et {
        Some(name) => match svc.sched.et_index(name) {
            Some(i) => i,
            None => return err(StatusCode::BAD_REQUEST, format!("no ET named {name}")),
        },
        None => 0,
    };
    let e_id = svc.events.len();
    let bi_n = m.div_ceil(req.block);
    let bj_n = n.div_ceil(req.block);
    let mut units = Vec::with_capacity(bi_n * bj_n);
    for bi in 0..bi_n {
        for bj in 0..bj_n {
            let u = svc.unit_meta.len();
            svc.unit_meta.push(UnitMeta { event: e_id, bi, bj });
            units.push(u);
        }
    }
    svc.events.push(EventState {
        et,
        a: req.a_hash.clone(),
        b: req.b_hash.clone(),
        block: req.block,
        units: units.clone(),
        done: 0,
        results: HashMap::new(),
        submitted_at: now,
        completed_at: None,
        first_starve_at: None,
        reclaims: 0,
        last_reclaim: None,
        done_cost: 0.0,
    });
    for &u in &units {
        svc.sched.submit(et, u, 1.0, 0, &[]);
    }
    let to_wake = svc.wake_candidates();
    drop(svc);
    wake(&state, to_wake).await;
    Json(SubmitResp { e_id, units: units.len(), m, n, k }).into_response()
}

async fn event_status(State(state): State<Shared>, headers: HeaderMap, Path(e_id): Path<usize>) -> Response {
    if let Err(r) = check_admin(&state, &headers) {
        return r;
    }
    let svc = state.svc.lock().await;
    let ev = match svc.events.get(e_id) {
        Some(e) => e,
        None => return err(StatusCode::NOT_FOUND, "unknown event"),
    };
    let complete = ev.completed_at.is_some();
    let results = if complete {
        let mut v: Vec<(usize, String)> = ev.results.iter().map(|(u, h)| (*u, h.clone())).collect();
        v.sort();
        Some(v)
    } else {
        None
    };
    Json(EventStatus {
        e_id,
        status: if complete { "completed".into() } else if ev.done > 0 { "computing".into() } else { "queued".into() },
        done: ev.done,
        total: ev.units.len(),
        submitted_at: ev.submitted_at,
        completed_at: ev.completed_at,
        results,
    })
    .into_response()
}

async fn metrics(State(state): State<Shared>, headers: HeaderMap) -> Response {
    if let Err(r) = check_admin(&state, &headers) {
        return r;
    }
    let now = state.now();
    let mut svc = state.svc.lock().await;
    svc.finish_metrics(now);
    let truth = svc.node_truth();
    let w = svc.w;
    let text = match &state.config.data_dir {
        Some(d) => match svc.metrics.write_all(&d.join("metrics"), w, &truth) {
            Ok(s) => s,
            Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        },
        None => svc.metrics.summary(w, &truth),
    };
    text.into_response()
}

async fn health() -> &'static str {
    "ok"
}

pub fn app(state: Shared) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/nodes/register", post(register))
        .route("/pull", post(pull))
        .route("/report", post(report))
        .route("/blob/:hash", axum::routing::put(blob_put).get(blob_get))
        .route("/events", post(submit))
        .route("/events/:id", get(event_status))
        .route("/metrics", get(metrics))
        // Matrix blobs are tens of MB; axum's default limit is 2 MB.
        .layer(axum::extract::DefaultBodyLimit::max(2 * 1024 * 1024 * 1024))
        .with_state(state)
}

/// Timeout check every W/4, as in the simulator.
pub async fn timeout_task(state: Shared) {
    let tick = state.config.scheduler.w * state.config.scheduler.retry_fraction;
    let mut interval = tokio::time::interval(std::time::Duration::from_secs_f64(tick.max(0.05)));
    loop {
        interval.tick().await;
        let now = state.now();
        let mut svc = state.svc.lock().await;
        let reclaimed = svc.sched.check_timeouts(now);
        if reclaimed.is_empty() {
            continue;
        }
        for r in reclaimed {
            svc.note_reclaim(&r.units, now);
        }
        let to_wake = svc.wake_candidates();
        drop(svc);
        wake(&state, to_wake).await;
    }
}
