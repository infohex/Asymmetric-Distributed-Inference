# Prototype A Runbook

**English** | [中文](../zh-CN/prototype-a-runbook.md)

Three binaries: `adi-scheduler`, `adi-node`, `adi-submit`. One `cargo build --release` at the workspace root produces all of them under `target\release\` (Windows) or `target/release/` (Linux / macOS).

## 1. Scheduler (one machine)

```
copy crates\adi-scheduler\config\example.toml scheduler.toml
```

Edit `scheduler.toml`: `listen = "0.0.0.0:7700"`, `data_dir = "data"`, a random `admin_token`, `w = 1.0` (LAN) or `3.0` (with WAN nodes). Then:

```
target\release\adi-scheduler.exe scheduler.toml
```

Windows Firewall will prompt; allow private networks. For WAN nodes, forward port 7700 to this machine on the router, or put it behind a reverse proxy.

## 2. Nodes (every computing machine)

```
target\release\adi-node.exe --scheduler http://<scheduler-ip>:7700 --name <unique-name>
```

`--name` must be unique; two agents with the same name evict each other. Threads default to the physical core count; lower `--threads` to make a "slow node". The log prints one line per round: U count, compute time, measured U/s.

## 3. Submit and verify (any machine, usually the scheduler's)

Single-machine sanity (acceptance check 1):

```
target\release\adi-submit.exe run --scheduler http://127.0.0.1:7700 --admin-token <token> --m 4096 --k 4096 --n 4096 --block 128 --events 2 --verify
```

Expected: 1024 U per Event and `all 1024 blocks bit-identical to the reference`. The reference is computed on the submitting machine; 4096³ takes tens of seconds to minutes, so use `--verify` for the first run and whenever a result is in doubt.

Two-machine run (acceptance check 2):

```
target\release\adi-submit.exe run --scheduler http://<scheduler-ip>:7700 --admin-token <token> --m 4096 --k 4096 --n 4096 --block 128 --events 10 --out results\real\<name>
```

Prints per-Event makespans and the scheduler summary (simulator format); `results\real\<name>\summary.txt` and `events.csv` are written and go into the repository.

## 4. Comparison with the simulator (acceptance check 3)

Take each node's `r_est` from the summary's per-node section, B from the node log's first round (bytes ÷ seconds) and latency from an empty round trip, and write `sim/scenarios/real-<name>.toml`: node `r_true = r_est`, `b_true = B`, `latency`, `c = threads`; workload `events = 10, units_per_event = 1024, arrival = "sequential"`. Run `sim run` and compare total time: measured ÷ simulated ≤ 1.15 passes.

## 5. Churn (acceptance check 4)

Ctrl+C one node mid-Event, restart it under the same name after 10 s. The summary should show `units reissued > 0`, V4 PASS, and the Event still bit-identical (`--verify`).
