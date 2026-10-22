//! Matrix blob format shared by scheduler, node and submitter:
//! little-endian `u32 rows, u32 cols`, then `f32[rows * cols]` row-major.

#[derive(Debug, Clone, PartialEq)]
pub struct Matrix {
    pub rows: usize,
    pub cols: usize,
    pub data: Vec<f32>,
}

impl Matrix {
    pub fn zeros(rows: usize, cols: usize) -> Matrix {
        Matrix { rows, cols, data: vec![0.0; rows * cols] }
    }

    #[inline]
    pub fn at(&self, r: usize, c: usize) -> f32 {
        self.data[r * self.cols + c]
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(8 + self.data.len() * 4);
        v.extend_from_slice(&(self.rows as u32).to_le_bytes());
        v.extend_from_slice(&(self.cols as u32).to_le_bytes());
        for x in &self.data {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v
    }

    pub fn decode(blob: &[u8]) -> Option<Matrix> {
        if blob.len() < 8 {
            return None;
        }
        let rows = u32::from_le_bytes(blob[0..4].try_into().ok()?) as usize;
        let cols = u32::from_le_bytes(blob[4..8].try_into().ok()?) as usize;
        if blob.len() != 8 + rows * cols * 4 {
            return None;
        }
        let mut data = Vec::with_capacity(rows * cols);
        for ch in blob[8..].chunks_exact(4) {
            data.push(f32::from_le_bytes([ch[0], ch[1], ch[2], ch[3]]));
        }
        Some(Matrix { rows, cols, data })
    }

    /// Row-major transpose.
    pub fn transposed(&self) -> Matrix {
        let mut t = Matrix::zeros(self.cols, self.rows);
        for r in 0..self.rows {
            for c in 0..self.cols {
                t.data[c * self.rows + r] = self.data[r * self.cols + c];
            }
        }
        t
    }
}

/// Dimensions only, without decoding the data.
pub fn dims(blob: &[u8]) -> Option<(usize, usize)> {
    if blob.len() < 8 {
        return None;
    }
    let rows = u32::from_le_bytes(blob[0..4].try_into().ok()?) as usize;
    let cols = u32::from_le_bytes(blob[4..8].try_into().ok()?) as usize;
    if blob.len() != 8 + rows * cols * 4 {
        return None;
    }
    Some((rows, cols))
}

/// Strict block product: C[bi, bj] of A (M×K) × B (K×N), with `bt` = B
/// transposed (N×K). For every output element the K terms are accumulated in
/// ascending order in f32 with separate multiply and add, so the result is
/// bit-identical on every platform. Edge blocks are clipped.
pub fn block_matmul_strict(a: &Matrix, bt: &Matrix, bi: usize, bj: usize, block: usize) -> Matrix {
    let m = a.rows;
    let k = a.cols;
    let n = bt.rows;
    debug_assert_eq!(bt.cols, k);
    let r0 = bi * block;
    let c0 = bj * block;
    let rows = block.min(m.saturating_sub(r0));
    let cols = block.min(n.saturating_sub(c0));
    let mut out = Matrix::zeros(rows, cols);
    for i in 0..rows {
        let arow = &a.data[(r0 + i) * k..(r0 + i + 1) * k];
        for j in 0..cols {
            let brow = &bt.data[(c0 + j) * k..(c0 + j + 1) * k];
            let mut acc = 0.0f32;
            for t in 0..k {
                acc += arow[t] * brow[t];
            }
            out.data[i * cols + j] = acc;
        }
    }
    out
}

/// Deterministic test matrix: values in [-1, 1) from a 64-bit xorshift seed.
pub fn generate(rows: usize, cols: usize, seed: u64) -> Matrix {
    let mut s = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
    let mut data = Vec::with_capacity(rows * cols);
    for _ in 0..rows * cols {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        data.push(((s >> 40) as f32 / (1u64 << 23) as f32) - 1.0);
    }
    Matrix { rows, cols, data }
}
