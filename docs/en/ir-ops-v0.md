# IR Operator Set v0

**English** | [中文](../zh-CN/ir-ops-v0.md)

Draft for review · 2026-10-22

The IR is defined at the tensor operator graph level. v0 has **30 operators**, aimed at covering blocked matrix computation and the forward pass of Transformer inference; LayerNorm, RMSNorm, attention and the like are composed from these and get no operator of their own.

## General conventions

- **Data types**: f32, f16, bf16, i32, bool. All v0 accumulation is in f32 regardless of input precision.
- **Shapes**: every operator subgraph in a U has fully static shapes at compile time; dynamic shapes are not supported.
- **Layout**: row-major (last dimension contiguous).
- **Broadcasting**: only `broadcast_to` broadcasts; elementwise operators require identical input shapes. The compiler inserts `broadcast_to`; runtimes never broadcast implicitly.
- **Two determinism profiles** (declared in the U's output_schema):
  - **strict**: bit-identical under the reduction orders in the table below; CPU runtimes must support it; used for sampled verification and intermediates on serial chains.
  - **fast**: runtimes may use any reduction order; cross-node comparison uses tolerance ε (default relative 1e-3); GPU runtimes use this profile by default.

## Operator list

| # | Operator | Inputs → output | Determinism convention (strict) |
| --- | --- | --- | --- |
| **Linear algebra** | | | |
| 1 | matmul | [.., M, K] × [.., K, N] → [.., M, N] | Per output element, accumulate over K in ascending order (f32) |
| **Elementwise (identical input shapes required)** | | | |
| 2 | add | a, b → a + b | No reduction |
| 3 | sub | a, b → a − b | No reduction |
| 4 | mul | a, b → a × b | No reduction |
| 5 | div | a, b → a ÷ b | No reduction; division by zero per IEEE 754 |
| 6 | neg | a → −a | No reduction |
| 7 | exp | a → eᵃ | Correctly rounded implementation (libm grade); runtimes may not use fast approximations |
| 8 | log | a → ln a | Same |
| 9 | sqrt | a → √a | IEEE 754 requires correct rounding; inherently deterministic |
| 10 | rsqrt | a → 1 ÷ √a | Defined as div(1, sqrt(a)); runtimes may not use hardware approximation instructions |
| 11 | maximum | a, b → max(a, b) | No reduction; NaN rule fixed as "NaN if either input is NaN" |
| 12 | minimum | a, b → min(a, b) | Same |
| 13 | where | cond(bool), a, b → cond ? a : b | No reduction |
| **Reductions** | | | |
| 14 | reduce_sum | a, axis → sum along axis | Accumulate along axis in ascending order (f32) |
| 15 | reduce_max | a, axis → max along axis | Order-independent (max commutes); NaN rule as maximum |
| 16 | reduce_min | a, axis → min along axis | Same |
| 17 | argmax | a, axis → index of max (i32) | Ties resolve to the smallest index |
| **Shape (no value change)** | | | |
| 18 | reshape | a, new_shape → same data, new shape | Element count must match; pure view |
| 19 | transpose | a, perm → dimensions permuted by perm | Pure permutation |
| 20 | slice | a, start, end, step → sub-tensor | Pure copy |
| 21 | concat | a₁..aₙ, axis → concatenated along axis | Pure copy |
| 22 | broadcast_to | a, shape → broadcast to shape | Pure copy |
| 23 | gather | a, indices, axis → indexed take | Out-of-range indices are a compile-time error; not checked at runtime |
| **Activations (fixed formulas)** | | | |
| 24 | relu | a → max(a, 0) | No reduction |
| 25 | sigmoid | a → 1 ÷ (1 + e⁻ᵃ) | Expanded into exp, add, div in fixed order |
| 26 | tanh | a → tanh a | Correctly rounded implementation |
| 27 | gelu | a → 0.5·a·(1 + tanh(√(2/π)·(a + 0.044715·a³))) | Fixed to the tanh approximation, not the erf form; constants as f32 literals |
| 28 | silu | a → a · sigmoid(a) | Fixed expansion order |
| 29 | softmax | a, axis → softmax along axis | Fixed as: reduce_max, subtract, exp, reduce_sum (ascending accumulation), div |
| **Utility** | | | |
| 30 | cast | a, dtype → precision conversion | f32→f16/bf16 round-to-nearest-even; →i32 truncates toward zero |

## Why these are left out

| Not included | Reason |
| --- | --- |
| layernorm / rmsnorm | Composed from reduce_sum, div, sub, mul, rsqrt; the compiler can recognize and fuse the pattern, no IR operator needed |
| attention | Composed from matmul, softmax, transpose |
| conv | v0 targets matrix and Transformer inference; convolution can come in v1 via im2col + matmul or a dedicated operator |
| random numbers (dropout etc.) | Not needed for inference; revisit for training (shape C), and they break idempotency |
| control flow (loop / if) | Never inside a U; expressed by E spawning E |
| dynamic shapes | Break the predictability the granularity rule relies on |
| constants / parameter inputs | Not operators; part of the U's input slice (inlined or content-addressed) |

## Hardware tier mapping

| Tier | Required operators | Notes |
| --- | --- | --- |
| General (CPU) | All 30, both strict and fast | Serves as the verification reference |
| General GPU | All 30 in fast; strict optional | A GPU node without strict support cannot take U declared strict |
| Specialized | Not defined in v0 | Opened when a need beyond the 30 operators appears |

## For your confirmation

- [ ] Is the 30-operator scope sufficient; is any computation you expect left uncovered?
- [ ] gelu fixed to the tanh approximation (matching most open models), or also support the erf form (one more operator)?
- [ ] Is the fast-profile default tolerance of relative 1e-3 appropriate; f16 inputs may need 1e-2.
