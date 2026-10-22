# 形态 A 原型运行手册

[English](../en/prototype-a-runbook.md) | **中文**

三个二进制：`adi-scheduler`（调度端）、`adi-node`（节点代理）、`adi-submit`（提交与校验）。在工作区根目录 `cargo build --release` 一次生成，在 `target\release\`（Windows）或 `target/release/`（Linux / macOS）。

## 1. 调度端（一台机器）

```
copy crates\adi-scheduler\config\example.toml scheduler.toml
```

编辑 `scheduler.toml`：`listen = "0.0.0.0:7700"`，`data_dir = "data"`，`admin_token` 设一个随机串，`w = 1.0`（局域网）或 `3.0`（含公网节点）。然后：

```
target\release\adi-scheduler.exe scheduler.toml
```

Windows 防火墙会弹窗，允许专用网络。公网节点要接入时，在路由器上把 7700 转发到这台机器，或放在反向代理后。

## 2. 节点（每台参与计算的机器）

```
target\release\adi-node.exe --scheduler http://<调度端IP>:7700 --name <唯一名字>
```

`--name` 全网唯一，同名会互相踢掉。默认线程数 = 物理核数，可用 `--threads` 改小来制造"慢节点"。日志每轮打印一行：U 数、计算时间、实测 U/s。

## 3. 提交与校验（任意一台机器，通常是调度端那台）

单机 sanity（验证第 1 项）：

```
target\release\adi-submit.exe run --scheduler http://127.0.0.1:7700 --admin-token <token> --m 4096 --k 4096 --n 4096 --block 128 --events 2 --verify
```

预期：每个 E 1024 个 U，`all 1024 blocks bit-identical to the reference`。参考计算在提交机本地做，4096³ 要几十秒到几分钟，只在第一次和怀疑结果时开 `--verify`。

双机实测（验证第 2 项）：

```
target\release\adi-submit.exe run --scheduler http://<调度端IP>:7700 --admin-token <token> --m 4096 --k 4096 --n 4096 --block 128 --events 10 --out results\real\<名字>
```

结束时打印每个 E 的用时和调度端的 summary（与模拟器同格式）；`results\real\<名字>\summary.txt` 和 `events.csv` 落盘，提交到仓库。

## 4. 与模拟器对照（验证第 3 项）

从 summary 的 per node 一节取每个节点的 `r_est`，从节点日志取首轮的取数时间（bytes ÷ 秒 = B）和一个空轮的往返（latency），写一个 `sim/scenarios/real-<名字>.toml`：节点 `r_true = r_est`，`b_true = B`，`latency`，`c = 线程数`；workload `events = 10, units_per_event = 1024, arrival = "sequential"`。跑 `sim run`，比较总用时：实测 ÷ 模拟 ≤ 1.15 通过。

## 5. 掉线（验证第 4 项）

E 运行中 Ctrl+C 掉一个节点，10 秒后重启同名。summary 里应出现 `units reissued > 0`，V4 通过，E 仍逐位正确（加 `--verify` 确认）。
