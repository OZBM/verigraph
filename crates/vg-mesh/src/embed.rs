//! Pluggable intent embedding model and cosine similarity for Mesh semantic search.

/// Pluggable text embedding trait for intent indexing and ranking (P7.2).
pub trait Embedder: Send + Sync {
    /// Stable identifier of the embedding model (stored in the `embeddings` table).
    fn model_id(&self) -> &str;
    /// Output embedding dimension.
    fn dim(&self) -> usize;
    /// Deterministic embedding vector for `text`.
    fn embed(&self, text: &str) -> Vec<f32>;
}

/// Deterministic default embedder: signed feature hashing over lower-cased words and
/// character 3-grams, L2-normalized to unit length (zero vector on empty input).
#[derive(Clone, Debug)]
pub struct DefaultEmbedder {
    dim: usize,
}

impl DefaultEmbedder {
    pub const DEFAULT_DIM: usize = 256;
    pub const MODEL_ID: &'static str = "hash-bow-trigram-v1";

    pub fn new(dim: usize) -> Self {
        assert!(dim > 0, "embedding dimension must be > 0");
        Self { dim }
    }
}

impl Default for DefaultEmbedder {
    fn default() -> Self {
        Self::new(Self::DEFAULT_DIM)
    }
}

/// FNV-1a 64-bit hash with a domain prefix byte for deterministic feature hashing.
fn fnv1a_64(domain: u8, bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    h ^= u64::from(domain);
    h = h.wrapping_mul(0x100000001b3);
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    // SplitMix64 finalizer for uniform bucket + sign bit distribution.
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58476d1ce4e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d049bb133111eb);
    h ^= h >> 31;
    h
}

impl Embedder for DefaultEmbedder {
    fn model_id(&self) -> &str {
        Self::MODEL_ID
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let mut vec = vec![0.0f32; self.dim];
        let lower = text.to_ascii_lowercase();
        let mut word = Vec::with_capacity(32);

        let mut add_token = |tok: &[u8]| {
            if tok.is_empty() {
                return;
            }
            let hw = fnv1a_64(1, tok);
            let bucket = (hw as usize) % self.dim;
            let sign = if (hw >> 63) == 0 { 1.0f32 } else { -1.0f32 };
            vec[bucket] += sign;

            // Character 3-grams over `^<tok>$`.
            let mut padded = Vec::with_capacity(tok.len() + 2);
            padded.push(b'^');
            padded.extend_from_slice(tok);
            padded.push(b'$');
            for tri in padded.windows(3) {
                let ht = fnv1a_64(2, tri);
                let tb = (ht as usize) % self.dim;
                let ts = if (ht >> 63) == 0 { 0.5f32 } else { -0.5f32 };
                vec[tb] += ts;
            }
        };

        for b in lower.bytes() {
            if b.is_ascii_alphanumeric() || b == b'_' {
                word.push(b);
            } else if !word.is_empty() {
                add_token(&word);
                word.clear();
            }
        }
        if !word.is_empty() {
            add_token(&word);
        }

        let norm_sq: f32 = vec.iter().map(|x| x * x).sum();
        if norm_sq > 0.0 {
            let inv = 1.0f32 / norm_sq.sqrt();
            for x in &mut vec {
                *x *= inv;
            }
        }
        vec
    }
}

/// Fast cosine similarity for already L2-normalized embedding vectors (or dot product if unit).
#[inline]
pub fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut s0 = 0.0f32;
    let mut s1 = 0.0f32;
    let mut s2 = 0.0f32;
    let mut s3 = 0.0f32;
    let mut i = 0;
    let n = a.len();
    while i + 4 <= n {
        s0 += a[i] * b[i];
        s1 += a[i + 1] * b[i + 1];
        s2 += a[i + 2] * b[i + 2];
        s3 += a[i + 3] * b[i + 3];
        i += 4;
    }
    let mut sum = (s0 + s1) + (s2 + s3);
    while i < n {
        sum += a[i] * b[i];
        i += 1;
    }
    sum
}

/// General cosine similarity normalizing by both vector norms.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot = dot_product(a, b);
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// Encodes a float vector as little-endian bytes for SQLite BLOB storage.
pub fn encode_vec(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for &x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Decodes little-endian float bytes from SQLite BLOB storage.
pub fn decode_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_unit_norm_and_similarity() {
        let e = DefaultEmbedder::default();
        assert_eq!(e.dim(), 256);
        assert_eq!(e.embed(""), vec![0.0; 256]);

        let v1 = e.embed("IEEE 802.3 CRC-32 checksum over a byte slice");
        let v2 = e.embed("CRC32 digest over byte slice");
        let v3 = e.embed("Parse HTTP headers and write socket telemetry");

        assert_eq!(v1, e.embed("IEEE 802.3 CRC-32 checksum over a byte slice"));
        let norm: f32 = v1.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);

        let sim_close = cosine_similarity(&v1, &v2);
        let sim_far = cosine_similarity(&v1, &v3);
        assert!(sim_close > sim_far, "close={sim_close}, far={sim_far}");
        assert!((dot_product(&v1, &v1) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn blob_round_trip() {
        let v = vec![0.25f32, -0.5, 1.0, 0.0];
        let b = encode_vec(&v);
        assert_eq!(decode_vec(&b), v);
    }
}
