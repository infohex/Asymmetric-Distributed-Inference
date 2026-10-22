# IR 算子集 v0

[English](../en/ir-ops-v0.md) | **中文**

草案待审 · 2026-10-22

IR 定在张量算子图层级。v0 共 **30 个算子**，目标是覆盖分块矩阵计算和 Transformer 推理的前向过程；LayerNorm、RMSNorm、注意力等都由这些算子组合而成，不单设算子。

## 通用约定

- **数据类型**：f32、f16、bf16、i32、bool。v0 的累加一律用 f32，无论输入精度。
- **形状**：每个 U 的算子子图在编译时形状全部静态，不支持动态形状。
- **布局**：行主序（最后一维连续）。
- **广播**：只有 `broadcast_to` 做广播，逐元素算子要求输入形状完全一致。编译器负责插入 `broadcast_to`，运行时不做隐式广播。
- **两种确定性档**（在 U 的 output_schema 里声明）：
  - **strict**：按下表的归约顺序逐位一致，CPU 运行时必须支持；用于抽样校验和串行链条中间结果。
  - **fast**：运行时可用任意归约顺序，跨节点比对用容差 ε（默认相对 1e-3）；GPU 运行时默认走这一档。

## 算子清单

| # | 算子 | 输入 → 输出 | strict 档的确定性约定 |
| --- | --- | --- | --- |
| **线性代数** | | | |
| 1 | matmul | [.., M, K] × [.., K, N] → [.., M, N] | 对每个输出元素，沿 K 升序逐项累加（f32） |
| **逐元素（输入形状必须一致）** | | | |
| 2 | add | a, b → a + b | 无归约 |
| 3 | sub | a, b → a − b | 无归约 |
| 4 | mul | a, b → a × b | 无归约 |
| 5 | div | a, b → a ÷ b | 无归约；除零结果按 IEEE 754 |
| 6 | neg | a → −a | 无归约 |
| 7 | exp | a → eᵃ | 使用正确舍入的实现（如 libm 级），运行时不得用快速近似 |
| 8 | log | a → ln a | 同上 |
| 9 | sqrt | a → √a | IEEE 754 要求正确舍入，天然确定 |
| 10 | rsqrt | a → 1 ÷ √a | 定义为 div(1, sqrt(a))，运行时不得用硬件近似指令 |
| 11 | maximum | a, b → max(a, b) | 无归约；NaN 传播规则固定为"任一为 NaN 则输出 NaN" |
| 12 | minimum | a, b → min(a, b) | 同上 |
| 13 | where | cond(bool), a, b → cond ? a : b | 无归约 |
| **归约** | | | |
| 14 | reduce_sum | a, axis → 沿 axis 求和 | 沿 axis 升序逐项累加（f32） |
| 15 | reduce_max | a, axis → 沿 axis 最大 | 顺序无关（max 可交换），NaN 规则同 maximum |
| 16 | reduce_min | a, axis → 沿 axis 最小 | 同上 |
| 17 | argmax | a, axis → 最大值下标（i32） | 多个相同最大值取最小下标 |
| **形状（不改变数值）** | | | |
| 18 | reshape | a, new_shape → 同数据新形状 | 元素数相等，纯视图 |
| 19 | transpose | a, perm → 按 perm 重排维度 | 纯重排 |
| 20 | slice | a, start, end, step → 子张量 | 纯复制 |
| 21 | concat | a₁..aₙ, axis → 沿 axis 拼接 | 纯复制 |
| 22 | broadcast_to | a, shape → 广播到 shape | 纯复制 |
| 23 | gather | a, indices, axis → 按下标取 | 越界下标为编译期错误，运行时不检查 |
| **激活（固定公式）** | | | |
| 24 | relu | a → max(a, 0) | 无归约 |
| 25 | sigmoid | a → 1 ÷ (1 + e⁻ᵃ) | 展开为 exp、add、div 的固定顺序 |
| 26 | tanh | a → tanh a | 正确舍入实现 |
| 27 | gelu | a → 0.5·a·(1 + tanh(√(2/π)·(a + 0.044715·a³))) | 固定用 tanh 近似式，不用 erf 式；常数为 f32 字面量 |
| 28 | silu | a → a · sigmoid(a) | 展开顺序固定 |
| 29 | softmax | a, axis → 沿 axis 的 softmax | 固定为：先 reduce_max，减去后 exp，再 reduce_sum（升序累加），再 div |
| **辅助** | | | |
| 30 | cast | a, dtype → 转换精度 | f32→f16/bf16 用最近偶数舍入；→i32 向零截断 |

## 为什么没有这些

| 未收录 | 理由 |
| --- | --- |
| layernorm / rmsnorm | 由 reduce_sum、div、sub、mul、rsqrt 组合，编译器可整体识别后再融合，不必是 IR 算子 |
| attention | 由 matmul、softmax、transpose 组合 |
| conv | v0 面向矩阵类和 Transformer 推理；卷积可在 v1 以 im2col + matmul 实现，或加专用算子 |
| 随机数（dropout 等） | 推理不需要；训练阶段（形态 C）再议，且会破坏幂等性 |
| 控制流（loop / if） | 不进 U，由 E 衍生 E 表达 |
| 动态形状 | 破坏粒度规则的可预测性 |
| 常量 / 参数输入 | 不是算子，是 U 输入切片的一部分（内联或内容地址引用） |

## 硬件档映射

| 硬件档 | 必须支持的算子 | 备注 |
| --- | --- | --- |
| 通用（CPU） | 全部 30 个，strict 和 fast 两档 | 作为校验基准 |
| GPU 通用 | 全部 30 个，fast 档；strict 档可选 | 不支持 strict 的 GPU 节点不能取声明 strict 的 U |
| 专用 | v0 不定义 | 等出现 30 个算子以外的需求时再开 |

## 待你确认

- [ ] 30 个算子的范围是否够用，有没有你预期的计算类型覆盖不到。
- [ ] gelu 固定用 tanh 近似式（与大多数开源模型一致），还是同时支持 erf 式（多一个算子）。
- [ ] fast 档默认容差相对 1e-3 是否合适；f16 输入时可能需要放宽到 1e-2。
