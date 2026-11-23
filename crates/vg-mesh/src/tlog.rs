//! RFC 6962 Merkle transparency log, C2SP signed notes, checkpoints, and static tiles.
//!
//! References:
//! - RFC 6962 §2.1 (Merkle Hash Trees, inclusion & consistency proofs)
//! - c2sp.org/signed-note
//! - c2sp.org/tlog-checkpoint
//! - c2sp.org/tlog-tiles (height 8 / width 256)

use std::fs;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::base64;

/// Tile height per `c2sp.org/tlog-tiles`: 8 levels per tile (width = 256 hashes).
pub const TILE_HEIGHT: u8 = 8;
pub const TILE_WIDTH: usize = 256;

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    let d = h.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&d);
    out
}

/// RFC 6962 §2.1: `SHA-256(0x00 || data)`.
pub fn leaf_hash(data: &[u8]) -> [u8; 32] {
    sha256(&[&[0x00], data])
}

/// RFC 6962 §2.1: `SHA-256(0x01 || left || right)`.
pub fn node_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    sha256(&[&[0x01], left, right])
}

/// RFC 6962 §2.1: `MTH({}) = SHA-256("")`.
pub fn empty_root() -> [u8; 32] {
    sha256(&[b""])
}

/// Largest power of two strictly less than `n` (`n >= 2`).
fn largest_pow2_lt(n: usize) -> usize {
    debug_assert!(n >= 2);
    1usize << (usize::BITS - 1 - (n - 1).leading_zeros())
}

/// Computes the RFC 6962 Merkle tree root over `leaves` (already leaf-hashed).
pub fn merkle_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    match leaves.len() {
        0 => empty_root(),
        1 => leaves[0],
        n => {
            let k = largest_pow2_lt(n);
            let l = merkle_root(&leaves[..k]);
            let r = merkle_root(&leaves[k..]);
            node_hash(&l, &r)
        }
    }
}

/// RFC 6962 §2.1.1 audit/inclusion path for leaf `index` in a tree of `tree_size` leaves.
pub fn inclusion_proof(
    leaves: &[[u8; 32]],
    index: usize,
    tree_size: usize,
) -> Result<Vec<[u8; 32]>, String> {
    if tree_size > leaves.len() {
        return Err("tree_size exceeds available leaves".into());
    }
    if index >= tree_size {
        return Err(format!(
            "index {index} out of bounds for tree_size {tree_size}"
        ));
    }
    Ok(path_rec(&leaves[..tree_size], index))
}

fn path_rec(leaves: &[[u8; 32]], index: usize) -> Vec<[u8; 32]> {
    let n = leaves.len();
    if n <= 1 {
        return Vec::new();
    }
    let k = largest_pow2_lt(n);
    if index < k {
        let mut p = path_rec(&leaves[..k], index);
        p.push(merkle_root(&leaves[k..]));
        p
    } else {
        let mut p = path_rec(&leaves[k..], index - k);
        p.push(merkle_root(&leaves[..k]));
        p
    }
}

/// Verifies an RFC 6962 inclusion proof for `leaf` (leaf hash) at `index` in a tree of `tree_size`.
pub fn verify_inclusion(
    leaf: &[u8; 32],
    index: u64,
    tree_size: u64,
    root: &[u8; 32],
    proof: &[[u8; 32]],
) -> bool {
    if tree_size == 0 || index >= tree_size {
        return false;
    }
    let mut fn_idx = index;
    let mut sn = tree_size - 1;
    let mut r = *leaf;
    for p in proof {
        if sn == 0 {
            return false;
        }
        if fn_idx & 1 == 1 || fn_idx == sn {
            r = node_hash(p, &r);
            while fn_idx != 0 && fn_idx & 1 == 0 {
                fn_idx >>= 1;
                sn >>= 1;
            }
        } else {
            r = node_hash(&r, p);
        }
        fn_idx >>= 1;
        sn >>= 1;
    }
    sn == 0 && &r == root
}

/// RFC 6962 §2.1.2 consistency proof between `first` and `second` (`0 <= first <= second`).
pub fn consistency_proof(
    leaves: &[[u8; 32]],
    first: usize,
    second: usize,
) -> Result<Vec<[u8; 32]>, String> {
    if second > leaves.len() {
        return Err("second size exceeds available leaves".into());
    }
    if first > second {
        return Err("first size exceeds second size".into());
    }
    if first == 0 || first == second {
        return Ok(Vec::new());
    }
    Ok(subproof(&leaves[..second], first, true))
}

fn subproof(leaves: &[[u8; 32]], m: usize, complete: bool) -> Vec<[u8; 32]> {
    let n = leaves.len();
    if m == n {
        if complete {
            Vec::new()
        } else {
            vec![merkle_root(leaves)]
        }
    } else {
        let k = largest_pow2_lt(n);
        if m <= k {
            let mut p = subproof(&leaves[..k], m, complete);
            p.push(merkle_root(&leaves[k..]));
            p
        } else {
            let mut p = subproof(&leaves[k..], m - k, false);
            p.push(merkle_root(&leaves[..k]));
            p
        }
    }
}

/// Verifies an RFC 6962 §2.1.4 consistency proof between `(first, first_root)` and `(second, second_root)`.
pub fn verify_consistency(
    first: u64,
    second: u64,
    first_root: &[u8; 32],
    second_root: &[u8; 32],
    proof: &[[u8; 32]],
) -> bool {
    if first > second {
        return false;
    }
    if first == 0 {
        return proof.is_empty();
    }
    if first == second {
        return first_root == second_root && proof.is_empty();
    }
    if proof.is_empty() {
        return false;
    }

    let mut idx = 0usize;
    let is_pow2 = first.is_power_of_two();
    let (mut fr, mut sr) = if is_pow2 {
        (*first_root, *first_root)
    } else {
        let h = proof[0];
        idx = 1;
        (h, h)
    };

    let mut fn_m = first - 1;
    let mut sn_m = second - 1;
    while fn_m & 1 == 1 {
        fn_m >>= 1;
        sn_m >>= 1;
    }

    for &c in &proof[idx..] {
        if sn_m == 0 {
            return false;
        }
        if fn_m & 1 == 1 || fn_m == sn_m {
            fr = node_hash(&c, &fr);
            sr = node_hash(&c, &sr);
            while fn_m != 0 && fn_m & 1 == 0 {
                fn_m >>= 1;
                sn_m >>= 1;
            }
        } else {
            sr = node_hash(&sr, &c);
        }
        fn_m >>= 1;
        sn_m >>= 1;
    }

    sn_m == 0 && &fr == first_root && &sr == second_root
}

/// C2SP signed-note Ed25519 key ID: first 4 bytes of `SHA-256(key_name || 0x0A || 0x01 || pubkey)`.
pub fn key_id(key_name: &str, vk: &VerifyingKey) -> [u8; 4] {
    let digest = sha256(&[key_name.as_bytes(), &[0x0A, 0x01], vk.as_bytes()]);
    [digest[0], digest[1], digest[2], digest[3]]
}

/// Validates a C2SP signed-note key name (non-empty, printable ASCII without spaces or `+`).
pub fn validate_key_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("key name must not be empty".into());
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_graphic() && b != b'+' && b != b' ')
    {
        return Err(format!("invalid C2SP key name: {name:?}"));
    }
    Ok(())
}

/// Formats a C2SP signed note (`c2sp.org/signed-note`) from an already-computed Ed25519 signature.
pub fn format_signed_note(
    body: &str,
    key_name: &str,
    vk: &VerifyingKey,
    sig: &Signature,
) -> String {
    let mut norm_body = body.to_string();
    if !norm_body.ends_with('\n') {
        norm_body.push('\n');
    }
    let kid = key_id(key_name, vk);
    let mut sig_blob = [0u8; 68];
    sig_blob[..4].copy_from_slice(&kid);
    sig_blob[4..].copy_from_slice(&sig.to_bytes());
    let b64 = base64::encode(&sig_blob);
    format!("{norm_body}\n\u{2014} {key_name} {b64}\n")
}

/// Signs `body` (normalized to end with `\n`) as a C2SP signed note.
pub fn sign_note(body: &str, key_name: &str, sk: &SigningKey) -> String {
    let mut norm_body = body.to_string();
    if !norm_body.ends_with('\n') {
        norm_body.push('\n');
    }
    let sig = sk.sign(norm_body.as_bytes());
    format_signed_note(&norm_body, key_name, &sk.verifying_key(), &sig)
}

/// Verifies a C2SP signed note against `(expected_key_name, vk)` and returns the signed text body.
pub fn verify_note(
    note: &str,
    expected_key_name: &str,
    vk: &VerifyingKey,
) -> Result<String, String> {
    let (raw_body, sig_section) = note
        .split_once("\n\n")
        .ok_or_else(|| "signed note missing blank separator line".to_string())?;
    let body = format!("{raw_body}\n");
    let expected_kid = key_id(expected_key_name, vk);

    for line in sig_section.lines() {
        let rest = line
            .strip_prefix("\u{2014} ")
            .ok_or_else(|| "invalid signature line prefix in signed note".to_string())?;
        let (name, b64) = rest
            .split_once(' ')
            .ok_or_else(|| "invalid signature line format in signed note".to_string())?;
        if name != expected_key_name {
            continue;
        }
        let raw = base64::decode(b64)?;
        if raw.len() != 68 {
            return Err("signed note signature payload must be 68 bytes".into());
        }
        if raw[..4] != expected_kid {
            continue;
        }
        let sig_bytes: [u8; 64] = raw[4..68]
            .try_into()
            .map_err(|_| "invalid signature length".to_string())?;
        let sig = Signature::from_bytes(&sig_bytes);
        vk.verify_strict(body.as_bytes(), &sig)
            .map_err(|e| format!("Ed25519 verification failed: {e}"))?;
        return Ok(body);
    }

    Err(format!(
        "no valid signature found for key {expected_key_name}"
    ))
}

/// A parsed C2SP `tlog-checkpoint`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub origin: String,
    pub size: u64,
    pub root: [u8; 32],
}

/// Formats a C2SP checkpoint body (`origin\nsize\nbase64(root)\n`).
pub fn format_checkpoint_body(origin: &str, size: u64, root: &[u8; 32]) -> String {
    format!("{origin}\n{size}\n{}\n", base64::encode(root))
}

/// Parses a C2SP checkpoint body (without signatures).
pub fn parse_checkpoint_body(body: &str) -> Result<Checkpoint, String> {
    let lines: Vec<&str> = body.lines().collect();
    if lines.len() < 3 {
        return Err("checkpoint body must have at least 3 lines".into());
    }
    let origin = lines[0].to_string();
    if origin.is_empty() {
        return Err("checkpoint origin must not be empty".into());
    }
    let size: u64 = lines[1]
        .parse()
        .map_err(|e| format!("invalid checkpoint tree size: {e}"))?;
    let root_bytes = base64::decode(lines[2])?;
    if root_bytes.len() != 32 {
        return Err("checkpoint root hash must be 32 bytes".into());
    }
    let mut root = [0u8; 32];
    root.copy_from_slice(&root_bytes);
    Ok(Checkpoint { origin, size, root })
}

/// Signs a C2SP checkpoint as a signed note.
pub fn sign_checkpoint(cp: &Checkpoint, key_name: &str, sk: &SigningKey) -> String {
    let body = format_checkpoint_body(&cp.origin, cp.size, &cp.root);
    sign_note(&body, key_name, sk)
}

/// Verifies a signed C2SP checkpoint note and checks its origin.
pub fn verify_checkpoint(
    note: &str,
    expected_origin: &str,
    key_name: &str,
    vk: &VerifyingKey,
) -> Result<Checkpoint, String> {
    let body = verify_note(note, key_name, vk)?;
    let cp = parse_checkpoint_body(&body)?;
    if cp.origin != expected_origin {
        return Err(format!(
            "checkpoint origin mismatch: expected {expected_origin}, got {}",
            cp.origin
        ));
    }
    Ok(cp)
}

/// Formats a `c2sp.org/tlog-tiles` relative path (`tile/<L>/<N>` or `tile/<L>/<N>.p/<W>`).
pub fn tile_path(level: u8, index: u64, partial_width: Option<u8>) -> String {
    let mut groups = Vec::new();
    let mut n = index;
    loop {
        groups.push(format!("{:03}", n % 1000));
        n /= 1000;
        if n == 0 {
            break;
        }
    }
    groups.reverse();
    let mut out = format!("tile/{level}");
    for (i, g) in groups.iter().enumerate() {
        out.push('/');
        if i + 1 < groups.len() {
            out.push('x');
        }
        out.push_str(g);
    }
    if let Some(w) = partial_width {
        out.push_str(&format!(".p/{w}"));
    }
    out
}

/// Validates a relative tile path from an HTTP GET `/tile/...` request.
pub fn is_valid_tile_rel_path(rel_path: &str) -> bool {
    if !rel_path.starts_with("tile/") || rel_path.contains("..") || rel_path.contains('\\') {
        return false;
    }
    rel_path
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'/' | b'.'))
}

/// Number of complete level-`level` nodes (each covering 256^level leaves) in a tree of `size` leaves.
fn level_count(size: usize, level: u8) -> usize {
    let shift = usize::from(level) * usize::from(TILE_HEIGHT);
    if shift >= usize::BITS as usize {
        0
    } else {
        size >> shift
    }
}

/// Encodes one tile at `(level, tile_idx)` containing `width` hashes (`1..=256`).
fn build_tile_bytes(leaves: &[[u8; 32]], level: u8, tile_idx: usize, width: usize) -> Vec<u8> {
    let base = tile_idx * TILE_WIDTH;
    let mut out = Vec::with_capacity(width * 32);
    for i in 0..width {
        let node_idx = base + i;
        let h = if level == 0 {
            leaves[node_idx]
        } else {
            // Each level-L hash is the root of 2^(8*L) leaves.
            let leaf_shift = usize::from(level) * usize::from(TILE_HEIGHT);
            let start = node_idx << leaf_shift;
            let end = start + (1usize << leaf_shift);
            merkle_root(&leaves[start..end])
        };
        out.extend_from_slice(&h);
    }
    out
}

/// Manages `<dir>/log/` containing C2SP static tiles and the latest signed checkpoint.
pub struct TileLog {
    origin: String,
    key_name: String,
    signing_key: SigningKey,
    leaves: Vec<[u8; 32]>,
    log_dir: PathBuf,
}

impl TileLog {
    pub fn open(
        log_dir: &Path,
        origin: impl Into<String>,
        key_name: impl Into<String>,
        signing_key: SigningKey,
        initial_leaves: Vec<[u8; 32]>,
    ) -> Result<Self, String> {
        fs::create_dir_all(log_dir).map_err(|e| format!("create log dir: {e}"))?;
        let tlog = Self {
            origin: origin.into(),
            key_name: key_name.into(),
            signing_key,
            leaves: initial_leaves,
            log_dir: log_dir.to_path_buf(),
        };
        if !tlog.checkpoint_path().exists() {
            tlog.write_checkpoint()?;
        }
        Ok(tlog)
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn key_name(&self) -> &str {
        &self.key_name
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    pub fn size(&self) -> u64 {
        self.leaves.len() as u64
    }

    pub fn leaves(&self) -> &[[u8; 32]] {
        &self.leaves
    }

    pub fn root(&self) -> [u8; 32] {
        merkle_root(&self.leaves)
    }

    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            origin: self.origin.clone(),
            size: self.leaves.len() as u64,
            root: self.root(),
        }
    }

    pub fn signed_checkpoint(&self) -> String {
        sign_checkpoint(&self.checkpoint(), &self.key_name, &self.signing_key)
    }

    pub fn checkpoint_path(&self) -> PathBuf {
        self.log_dir.join("checkpoint")
    }

    pub fn read_checkpoint(&self) -> Result<String, String> {
        fs::read_to_string(self.checkpoint_path()).map_err(|e| format!("read checkpoint: {e}"))
    }

    /// Appends one entry's raw payload to the log, updates affected tiles and checkpoint.
    /// Returns `(entry_index, leaf_hash, signed_checkpoint_text)`.
    pub fn append(&mut self, leaf_data: &[u8]) -> Result<(u64, [u8; 32], String), String> {
        let old_size = self.leaves.len();
        let lh = leaf_hash(leaf_data);
        let idx = old_size as u64;
        self.leaves.push(lh);
        let new_size = self.leaves.len();

        // Update every level whose complete-node count increased.
        let mut level: u8 = 0;
        loop {
            let old_cnt = level_count(old_size, level);
            let new_cnt = level_count(new_size, level);
            if new_cnt == 0 {
                break;
            }
            if new_cnt > old_cnt {
                let last_idx = new_cnt - 1;
                let tile_idx = (last_idx / TILE_WIDTH) as u64;
                let rem = new_cnt % TILE_WIDTH;
                if rem == 0 {
                    // Completed a full tile; remove previous partial `.p/255` if present.
                    let prev_partial = self.log_dir.join(tile_path(level, tile_idx, Some(255)));
                    if prev_partial.exists() {
                        let _ = fs::remove_file(&prev_partial);
                    }
                    let bytes =
                        build_tile_bytes(&self.leaves, level, tile_idx as usize, TILE_WIDTH);
                    self.write_tile_file(&tile_path(level, tile_idx, None), &bytes)?;
                } else {
                    let w = rem as u8;
                    if w > 1 {
                        let prev_partial =
                            self.log_dir.join(tile_path(level, tile_idx, Some(w - 1)));
                        if prev_partial.exists() {
                            let _ = fs::remove_file(&prev_partial);
                        }
                    }
                    let bytes = build_tile_bytes(&self.leaves, level, tile_idx as usize, rem);
                    self.write_tile_file(&tile_path(level, tile_idx, Some(w)), &bytes)?;
                }
            }
            level = level.saturating_add(1);
        }

        let signed_cp = self.write_checkpoint()?;
        Ok((idx, lh, signed_cp))
    }

    fn write_tile_file(&self, rel: &str, bytes: &[u8]) -> Result<(), String> {
        let path = self.log_dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("create tile dir: {e}"))?;
        }
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, bytes).map_err(|e| format!("write tile tmp: {e}"))?;
        fs::rename(&tmp, &path).map_err(|e| format!("rename tile: {e}"))?;
        Ok(())
    }

    fn write_checkpoint(&self) -> Result<String, String> {
        let text = self.signed_checkpoint();
        let path = self.checkpoint_path();
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, text.as_bytes()).map_err(|e| format!("write checkpoint tmp: {e}"))?;
        fs::rename(&tmp, &path).map_err(|e| format!("rename checkpoint: {e}"))?;
        Ok(text)
    }

    /// Reads a tile file (`tile/<L>/...`) with path-traversal protection.
    pub fn read_tile(&self, rel_path: &str) -> Result<Option<Vec<u8>>, String> {
        if !is_valid_tile_rel_path(rel_path) {
            return Err(format!("invalid tile path: {rel_path}"));
        }
        let path = self.log_dir.join(rel_path);
        match fs::read(&path) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("read tile: {e}")),
        }
    }

    /// Re-verifies the entire log on disk against stored `(entry_index, leaf_hash, payload)` rows:
    /// sequential indices, matching leaf hashes, valid signed checkpoint matching Merkle root,
    /// and matching level-0 tile contents.
    pub fn verify_integrity(
        &self,
        entries: &[(u64, [u8; 32], Vec<u8>)],
    ) -> Result<Checkpoint, String> {
        if entries.len() != self.leaves.len() {
            return Err(format!(
                "log entry count mismatch: db={}, memory={}",
                entries.len(),
                self.leaves.len()
            ));
        }
        for (expected_idx, (idx, recorded_lh, payload)) in entries.iter().enumerate() {
            if *idx != expected_idx as u64 {
                return Err(format!(
                    "log entries reordered or non-sequential: expected {expected_idx}, got {idx}"
                ));
            }
            let recomputed = leaf_hash(payload);
            if &recomputed != recorded_lh {
                return Err(format!("leaf hash mismatch at index {idx}"));
            }
            if *recorded_lh != self.leaves[expected_idx] {
                return Err(format!("in-memory leaf mismatch at index {idx}"));
            }
        }
        let cp_text = self.read_checkpoint()?;
        let cp = verify_checkpoint(
            &cp_text,
            &self.origin,
            &self.key_name,
            &self.verifying_key(),
        )?;
        if cp.size != entries.len() as u64 {
            return Err(format!(
                "checkpoint size {} disagrees with entries {}",
                cp.size,
                entries.len()
            ));
        }
        let expected_root = merkle_root(&self.leaves);
        if cp.root != expected_root {
            return Err("checkpoint root does not match recomputed Merkle root".into());
        }
        // Check level-0 tiles on disk match leaves.
        let total = self.leaves.len();
        let full_tiles = total / TILE_WIDTH;
        for t in 0..full_tiles {
            let rel = tile_path(0, t as u64, None);
            let bytes = self
                .read_tile(&rel)?
                .ok_or_else(|| format!("missing tile {rel}"))?;
            let expected = build_tile_bytes(&self.leaves, 0, t, TILE_WIDTH);
            if bytes != expected {
                return Err(format!("tile {rel} contents tampered"));
            }
        }
        let rem = total % TILE_WIDTH;
        if rem > 0 {
            let rel = tile_path(0, full_tiles as u64, Some(rem as u8));
            let bytes = self
                .read_tile(&rel)?
                .ok_or_else(|| format!("missing partial tile {rel}"))?;
            let expected = build_tile_bytes(&self.leaves, 0, full_tiles, rem);
            if bytes != expected {
                return Err(format!("partial tile {rel} contents tampered"));
            }
        }
        Ok(cp)
    }

    /// Checks consistency from a previously observed checkpoint `old_cp` to the current log.
    /// Rejects truncation (`current_size < old_cp.size`) and any rewritten history.
    pub fn verify_consistency_from(&self, old_cp: &Checkpoint) -> Result<(), String> {
        if old_cp.origin != self.origin {
            return Err("origin mismatch in consistency check".into());
        }
        let cur_size = self.leaves.len();
        let old_size = old_cp.size as usize;
        if cur_size < old_size {
            return Err(format!(
                "log truncated: previous size {old_size}, current size {cur_size}"
            ));
        }
        let cur_root = self.root();
        let proof = consistency_proof(&self.leaves, old_size, cur_size)?;
        if !verify_consistency(
            old_cp.size,
            cur_size as u64,
            &old_cp.root,
            &cur_root,
            &proof,
        ) {
            return Err("RFC 6962 consistency proof verification failed".into());
        }
        Ok(())
    }

    /// Computes the RFC 6962 inclusion proof for leaf `index`.
    pub fn proof_for(&self, index: u64) -> Result<Vec<[u8; 32]>, String> {
        inclusion_proof(&self.leaves, index as usize, self.leaves.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_sk(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    #[test]
    fn merkle_roots_and_all_inclusion_consistency_proofs() {
        assert_eq!(merkle_root(&[]), empty_root());

        let leaves: Vec<[u8; 32]> = (0..20usize)
            .map(|i| leaf_hash(format!("entry-{i}").as_bytes()))
            .collect();

        for size in 1..=leaves.len() {
            let root = merkle_root(&leaves[..size]);
            for idx in 0..size {
                let proof = inclusion_proof(&leaves, idx, size).unwrap();
                assert!(
                    verify_inclusion(&leaves[idx], idx as u64, size as u64, &root, &proof),
                    "inclusion failed for idx={idx}, size={size}"
                );
                // Tampered leaf must fail.
                let mut bad_leaf = leaves[idx];
                bad_leaf[0] ^= 1;
                assert!(!verify_inclusion(
                    &bad_leaf,
                    idx as u64,
                    size as u64,
                    &root,
                    &proof
                ));
            }
        }

        for first in 0..=leaves.len() {
            let first_root = merkle_root(&leaves[..first]);
            for second in first..=leaves.len() {
                let second_root = merkle_root(&leaves[..second]);
                let proof = consistency_proof(&leaves, first, second).unwrap();
                assert!(
                    verify_consistency(
                        first as u64,
                        second as u64,
                        &first_root,
                        &second_root,
                        &proof
                    ),
                    "consistency failed for first={first}, second={second}"
                );
                if first > 0 && first < second {
                    let mut bad_first = first_root;
                    bad_first[0] ^= 1;
                    assert!(!verify_consistency(
                        first as u64,
                        second as u64,
                        &bad_first,
                        &second_root,
                        &proof
                    ));
                }
            }
        }

        // Truncated log: second < first must be rejected.
        let r10 = merkle_root(&leaves[..10]);
        let r5 = merkle_root(&leaves[..5]);
        assert!(!verify_consistency(10, 5, &r10, &r5, &[]));
    }

    #[test]
    fn signed_note_and_checkpoint_roundtrip() {
        let sk = test_sk(42);
        let vk = sk.verifying_key();
        let note = sign_note("hello mesh", "mesh.example/key", &sk);
        let body = verify_note(&note, "mesh.example/key", &vk).unwrap();
        assert_eq!(body, "hello mesh\n");

        // Wrong key fails.
        let wrong_vk = test_sk(99).verifying_key();
        assert!(verify_note(&note, "mesh.example/key", &wrong_vk).is_err());

        // Tampered body fails.
        let tampered = note.replacen("hello mesh", "hello evil", 1);
        assert!(verify_note(&tampered, "mesh.example/key", &vk).is_err());

        let cp = Checkpoint {
            origin: "vg-mesh.local/v1".into(),
            size: 3,
            root: [7u8; 32],
        };
        let cp_note = sign_checkpoint(&cp, "mesh.example/key", &sk);
        let parsed =
            verify_checkpoint(&cp_note, "vg-mesh.local/v1", "mesh.example/key", &vk).unwrap();
        assert_eq!(parsed, cp);
        assert!(verify_checkpoint(&cp_note, "other.origin", "mesh.example/key", &vk).is_err());
    }

    #[test]
    fn tile_paths_and_256_boundary() {
        assert_eq!(tile_path(0, 0, None), "tile/0/000");
        assert_eq!(tile_path(0, 42, Some(7)), "tile/0/042.p/7");
        assert_eq!(tile_path(1, 1234, None), "tile/1/x001/234");
        assert_eq!(
            tile_path(0, 1_234_567, Some(255)),
            "tile/0/x001/x234/567.p/255"
        );

        let tmp = std::env::temp_dir().join(format!("vg-mesh-tlog-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let mut tlog =
            TileLog::open(&tmp, "vg-mesh.local", "mesh-log", test_sk(7), vec![]).unwrap();

        let mut entries = Vec::new();
        for i in 0..258usize {
            let payload = format!("submission-{i}").into_bytes();
            let (idx, lh, _) = tlog.append(&payload).unwrap();
            assert_eq!(idx, i as u64);
            entries.push((idx, lh, payload));
        }

        // Level 0 tile 000 should be full (256 * 32 = 8192 bytes), partial .p/255 deleted.
        assert!(tlog.read_tile("tile/0/000.p/255").unwrap().is_none());
        assert_eq!(
            tlog.read_tile("tile/0/000").unwrap().unwrap().len(),
            256 * 32
        );
        // Level 0 tile 001 has 2 entries -> partial .p/2 (64 bytes).
        assert_eq!(tlog.read_tile("tile/0/001.p/2").unwrap().unwrap().len(), 64);
        // Level 1 tile 000 has 1 entry (covering first 256 leaves) -> partial .p/1 (32 bytes).
        let lvl1 = tlog.read_tile("tile/1/000.p/1").unwrap().unwrap();
        assert_eq!(lvl1.len(), 32);
        assert_eq!(lvl1.as_slice(), &merkle_root(&tlog.leaves()[..256]));

        tlog.verify_integrity(&entries).unwrap();
        let cp = tlog.checkpoint();
        tlog.verify_consistency_from(&cp).unwrap();

        // Consistency from tree size 100 to 258.
        let cp100 = Checkpoint {
            origin: "vg-mesh.local".into(),
            size: 100,
            root: merkle_root(&tlog.leaves()[..100]),
        };
        tlog.verify_consistency_from(&cp100).unwrap();

        fs::remove_dir_all(&tmp).unwrap();
    }
}
