//! Content-addressed blob store: BLAKE3 hash → bytes, memory plus optional disk.

use bytes::Bytes;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

pub struct Store {
    mem: RwLock<HashMap<String, Bytes>>,
    dir: Option<PathBuf>,
}

pub fn hash_of(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

impl Store {
    pub fn new(dir: Option<PathBuf>) -> std::io::Result<Store> {
        if let Some(d) = &dir {
            std::fs::create_dir_all(d.join("blobs"))?;
        }
        Ok(Store { mem: RwLock::new(HashMap::new()), dir })
    }

    /// Store `data` under its own hash. Returns the hash.
    pub fn put(&self, data: Bytes) -> std::io::Result<String> {
        let h = hash_of(&data);
        if let Some(d) = &self.dir {
            let path = d.join("blobs").join(&h);
            if !path.exists() {
                std::fs::write(&path, &data)?;
            }
        }
        self.mem.write().unwrap().insert(h.clone(), data);
        Ok(h)
    }

    pub fn get(&self, hash: &str) -> Option<Bytes> {
        if let Some(b) = self.mem.read().unwrap().get(hash) {
            return Some(b.clone());
        }
        let d = self.dir.as_ref()?;
        if !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let data = std::fs::read(d.join("blobs").join(hash)).ok()?;
        let b = Bytes::from(data);
        self.mem.write().unwrap().insert(hash.to_string(), b.clone());
        Some(b)
    }

    pub fn has(&self, hash: &str) -> bool {
        self.get(hash).is_some()
    }
}

/// Matrix blob header: little-endian u32 rows, u32 cols, then f32 row-major.
pub fn matrix_dims(blob: &[u8]) -> Option<(usize, usize)> {
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
