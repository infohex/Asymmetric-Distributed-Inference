# adi-sim

Discrete-event simulator for the scheduling mechanism. Design: [docs/en/simulator-design.md](../docs/en/simulator-design.md) · [中文](../docs/zh-CN/simulator-design.md).

```
cargo build --release
./target/release/sim run scenarios/basic.toml            # results/basic/
./target/release/sim run scenarios/basic.toml --out /tmp/x
./target/release/sim sweep sweeps/noisy_drift_k.toml     # results/noisy_drift_sweep/sweep.csv
```

Exit code 0 when every criterion computed in this stage passes, 3 when one fails.

Outputs per scenario: `rounds.csv` (one row per round), `batches.csv` (one row per Event), `summary.txt` (per-node estimates vs truth, V1–V4 verdicts). `summary.txt` is committed for each scenario as the record of the run, and `sweep.csv` for each sweep; the per-round CSVs are not.

Stage status:

| Stage | Scope | Status |
| --- | --- | --- |
| 1 | engine, node model, pull / taper / EWMA, basic scenario, V1 / V2 | done |
| 2 | throughput noise (i.i.d. and AR(1) drift), σ and k, noisy scenarios, parameter sweep over seeds | done |
| 3 | hysteresis, flapping scenario, V3 | — |
| 4 | churn, timeout reclaim, churn scenario, V4 | — |
| 5 | dependency resolver, fanout-reduce, two hardware tiers | — |
