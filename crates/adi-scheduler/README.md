# adi-scheduler

Scheduler service: node registration, long-poll pull, report, content-addressed blob store, timeout reclaim, metrics. Every scheduling decision is made by `adi-core`; this crate adds the clock, the HTTP surface and the wake-up of parked nodes. Design: [docs/en/prototype-a-design.md](../../docs/en/prototype-a-design.md) · [中文](../../docs/zh-CN/prototype-a-design.md).

```
cargo build --release
cp crates/adi-scheduler/config/example.toml scheduler.toml   # edit listen, data_dir, admin_token, W
./target/release/adi-scheduler scheduler.toml
curl http://127.0.0.1:7700/health
curl http://127.0.0.1:7700/metrics          # summary; files under <data_dir>/metrics/
```

| Method | Path | Auth | Purpose |
| --- | --- | --- | --- |
| POST | /nodes/register | — | `{node_id, tier, c}` → `{node, token, w, retry_ms}`; re-registering the same name reclaims its in-flight round at once |
| POST | /pull | node token | `{node, wait_ms}` → a round (200) or 204 after wait_ms; 409 while a round is in flight |
| POST | /report | node token | `{node, round_id, started_at, finished_at, bytes_in, results:[{u_id, hash}]}` → `{accepted}`; a result counts only if its blob was uploaded first |
| PUT / GET | /blob/{hash} | token (PUT) | BLAKE3 content-addressed bytes; PUT rejects a body whose hash differs |
| POST | /events | admin token | `{a_hash, b_hash, block, et}` → `{e_id, units, m, n, k}`; A and B are matrix blobs (`u32 rows, u32 cols, f32[]` little-endian) |
| GET | /events/{id} | admin token | status, done / total, result hashes once complete |
| GET | /metrics | admin token | summary text in the simulator's format |

Tests: `cargo test --release -p adi-scheduler` runs two fake nodes (sleeping instead of computing) through three Events, and a reconnect-reclaim case.
