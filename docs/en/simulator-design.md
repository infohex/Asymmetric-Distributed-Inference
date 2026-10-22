# Scheduling Simulator — Design

**English** | [中文](../zh-CN/simulator-design.md)

Draft for review · 2026-10-22

## 1. Purpose

Before writing any network code, validate the scheduling mechanism itself with discrete-event simulation: whether fixed-window pulling, end-of-batch tapering, timeout reclaim, EWMA estimation and hysteresis really keep aggregation waiting within one W, and find sensible values for W, α, k and N.

The simulator does **not** do real computation, real networking or IR translation. Nodes are noisy mathematical models; U are records with a cost.

## 2. Acceptance criteria

A scenario passes only when all four hold:

| ID | Metric | Pass line |
| --- | --- | --- |
| V1 | In steady state, spread of round durations (return − issue), excluding slow-start and taper rounds | ≤ 0.2 × W |
| V2 | Per batch, tail wait (time the earliest-finished node idles until the batch completes) | ≤ 1 × W |
| V3 | Tier promotions/demotions for a node near a threshold (hysteresis on vs off) | ≥ 80% fewer with hysteresis |
| V4 | Time from the last reclaim touching an Event to that Event's completion | ≤ 2 × W + the Event's remaining cost at that moment ÷ total throughput of nodes online |

Also reported, without a pass line: scheduler messages per second, per-node idle ratio, share of reissued U.

## 3. Models

### 3.1 Nodes

Each node is generated from these parameters, all configurable in the scenario file:

| Parameter | Meaning | Noise model |
| --- | --- | --- |
| r_true | True compute throughput (U/s) | Multiplied each round by log-normal noise with configurable σ (throttling, background load) |
| B_true | True bandwidth (bytes/s) | Same |
| c | Concurrency | Fixed |
| online | Online / offline | Two-state Markov chain with configurable mean online and offline durations |
| hardware_tier | General / General GPU | Fixed |
| latency | Fixed per-round-trip overhead (s) | Fixed, added to each round's transfer time |

True time for a node to run a round of n U = latency + bytes ÷ B_true + n ÷ (r_true × this round's noise). The node does **not** know its r_true and neither does the scheduler; it can only infer from return times. That is the core of the simulation: the gap between estimate and truth is what the mechanism must absorb.

### 3.2 Workload

| Parameter | Meaning |
| --- | --- |
| Number of E, U per E | Batch size |
| Dependency shape | parallel / serial / fanout-reduce (N parallel + 1 reduce U) / custom DAG |
| U cost | Compute cost (relative units) and input size (bytes), uniform or drawn from a distribution |
| Target ET | By time ceiling and hardware tier |
| Arrival pattern | All at once / Poisson arrivals |

### 3.3 Scheduler

Implemented exactly as sections 7–9 of the design document:

- Node table: per-node EWMA estimates of r, σ, B; consecutive_pass; in_flight.
- ET: key = (hardware tier, t_max), FIFO ready queue.
- Pull: r_safe × W, minimum c; proportional tapering when a batch has less than one round left.
- Drop-down rule: a node checks the fastest ET it qualifies for first.
- Timeout: U not returned by issued_at + k × W are reclaimed and reissued.
- Hysteresis: promotion requires t ≤ t_max × (1 − band) for N consecutive rounds; one round of t > t_max demotes.
- Dependency resolver: U done → check dependents → enqueue.

Every scheduler decision uses only what it can observe (return times, byte counts), never node ground truth. The code separates the two with types so the simulator cannot "cheat".

## 4. Discrete-event engine

Single-threaded; a priority queue (`BinaryHeap`) ordered by timestamp. Event types:

| Event | Trigger | Handling |
| --- | --- | --- |
| NodePull | Node idle and online | Scheduler issues a round per the pull rules; if nothing is available, the node retries after W/4 |
| RoundDone | The round's true duration elapses | Update estimates; resolver unlocks dependents; node immediately NodePull |
| NodeOffline / NodeOnline | Markov chain | While offline, in-flight U are never returned (wait for timeout reclaim); on return, NodePull |
| TimeoutCheck | Every W/4 | Scan in_flight; reclaim expired U into their ET |
| EventArrival | Per arrival pattern | Compile E, hand to the resolver |

Random numbers use a fixed seed so any scenario is reproducible.

## 5. Output

- `results/<scenario>/rounds.csv`: one row per round — node, issued time, return time, count, estimated r, true r.
- `results/<scenario>/batches.csv`: one row per batch — start, end, tail wait, reissue count.
- `results/<scenario>/summary.txt`: V1–V4 values and pass / fail.
- Sweep mode: a grid over W, α, k, N, reporting V1–V4 per combination, used to pick defaults.

## 6. Scenarios

Four initial scenarios, one per acceptance criterion:

| Scenario | Key configuration | Looks at |
| --- | --- | --- |
| basic | 2 fast + 2 slow nodes, no noise, no churn, fully parallel E | V1, V2: does the mechanism hold under ideal conditions |
| noisy | As above + throughput noise σ = 0.3 | Degradation of V1, V2 under noise; sweep k |
| flapping | One node whose r_true sits right at the fast-ET threshold | V3; sweep N |
| churn | 8 nodes, mean 10 min online / 2 min offline | V4; sweep timeout multiplier |

Stage 5 adds the `deps` scenario: two hardware tiers (gpu / general) and three dependency shapes at once (parallel, a 300-step serial chain, 2000 parallel + one reduce U of cost 20); checks no tier violation, no U left waiting, V1/V2.

Later additions: Poisson arrivals, straggler duplication, critical-path priority.

## 7. Code layout (Rust)

```
sim/
  Cargo.toml
  src/
    main.rs        CLI: sim run <scenario.toml> / sim sweep <sweep.toml>
    engine.rs      event queue and clock
    node.rs        node ground-truth model (noise, online state)
    scheduler/
      mod.rs       node table, ET, pull, timeout, hysteresis
      resolver.rs  dependency resolution
      estimate.rs  EWMA, σ, r_safe
    workload.rs    E / U generation and dependency shapes
    metrics.rs     V1–V4 computation and CSV output
    scenario.rs    TOML config parsing
  scenarios/
    basic.toml  noisy.toml  flapping.toml  churn.toml
```

Dependencies limited to `serde` + `toml` (config), `rand` + `rand_distr` (noise), `csv` (output). No async runtime; single-threaded is enough.

The `scheduler/` layer is written against the design document's interfaces so the later prototype can reuse it directly: swap `engine.rs` for a real network and clock, and the scheduling logic stays unchanged.

## 8. Effort estimate

| Stage | Content | Estimate |
| --- | --- | --- |
| 1 | Engine + node model + basic scenario + V1/V2 | 1 day |
| 2 | Noise, EWMA, σ, noisy scenario + parameter sweep | 0.5 day |
| 3 | Hysteresis, flapping scenario, V3 | 0.5 day |
| 4 | Churn, timeout reclaim, churn scenario, V4 | 0.5 day |
| 5 | Dependency resolver, fanout-reduce, two hardware tiers | 0.5 day |

Each stage is committed separately with that stage's measured summary.txt attached.

## For your confirmation

- [ ] Are the V1–V4 pass lines reasonable, especially V2's "≤ 1 × W"?
- [ ] Do the four initial scenarios cover the cases you worry about most?
- [ ] Agree with separating `scheduler/` from the engine for reuse by the prototype?
- [ ] Deliver in five separate stages, or all at once?
