# adi-submit

Submitter: generates A (M×K) and B (K×N) deterministically from a seed, uploads them, submits `--events` Events one after another (each waits for the previous one to complete), and with `--verify` assembles C from the result blocks and compares it bit for bit with a strict reference computed locally. Prints per-Event makespans and the scheduler's metrics summary; `--out DIR` writes `summary.txt` and `events.csv` there.

```
adi-submit run --scheduler http://HOST:7700 [--admin-token T] --m 4096 --k 4096 --n 4096 --block 128 --events 10 [--seed 1] [--verify] [--out results/real/NAME]
adi-submit metrics --scheduler http://HOST:7700 [--admin-token T] [--out DIR]
```

With `--verify` the reference is computed on the submitting machine with the same per-element accumulation order as the nodes, so any mismatch is a real one.
