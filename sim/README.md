# adi-sim

Discrete-event simulator for the scheduling mechanism. Design: [docs/en/simulator-design.md](../docs/en/simulator-design.md) · [中文](../docs/zh-CN/simulator-design.md).

```
cargo build --release
./target/release/sim run scenarios/basic.toml            # results/basic/
./target/release/sim run scenarios/basic.toml --out /tmp/x
./target/release/sim sweep sweeps/noisy_drift_k.toml     # results/noisy_drift_sweep/sweep.csv
```

Exit code 0 when every criterion computed for the scenario passes, 3 when one fails.

Scenarios: `basic` (V1/V2), `noisy` and `noisy_drift` (noise), `flapping` (V3), `churn` (V4), `deps` (resolver, three dependency shapes, two hardware tiers). Sweeps under `sweeps/`.

Known limits of the model: idle nodes are woken by the scheduler the moment work appears (long-poll); a node that drops never returns its round (no partial results); unit cost is uniform except the reduce U; no data transfer beyond a per-round byte count.

Outputs per scenario: `rounds.csv` (one row per round), `batches.csv` (one row per Event), `summary.txt` (per-node estimates vs truth, V1–V4 verdicts). `summary.txt` is committed for each scenario as the record of the run, and `sweep.csv` for each sweep; the per-round CSVs are not.

Stage status:

| Stage | Scope | Status |
| --- | --- | --- |
| 1 | engine, node model, pull / taper / EWMA, basic scenario, V1 / V2 | done |
| 2 | throughput noise (i.i.d. and AR(1) drift), σ and k, noisy scenarios, parameter sweep over seeds | done |
| 3 | multiple ETs (tier × ceiling), eligibility with round-count + value-band hysteresis, flapping scenario, V3 | done |
| 4 | node churn (Markov on/off), timeout reclaim, reconnect reclaim, first-result-wins dedup, liveness by last contact, churn scenario, V4, timeout sweep | done |
| 5 | dependency resolver, serial / fanout-reduce shapes, two hardware tiers, long-poll wake-up, deps scenario | done |
