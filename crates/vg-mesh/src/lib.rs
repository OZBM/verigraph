//! VG-Mesh: content-addressed node registry, semantic & structural indexes,
//! behavioral query pipeline, 6-check ingestion gate, C2SP transparency log, and HTTP/1.1 API.

pub mod base64;
pub mod embed;
pub mod http;
pub mod ingest;
pub mod query;
pub mod store;
pub mod tlog;
pub mod types;

pub use embed::{DefaultEmbedder, Embedder, cosine_similarity};
pub use http::{MAX_BODY_BYTES, handle_stream, serve, serve_shared};
pub use ingest::sign_submission;
pub use store::{
    Mesh, SyntheticEntry, attestation_statement, name_ref_statement, sign_attestation,
    sign_name_ref, submission_statement,
};
pub use tlog::{
    Checkpoint, TileLog, consistency_proof, empty_root, inclusion_proof, key_id, leaf_hash,
    merkle_root, node_hash, parse_checkpoint_body, sign_checkpoint, sign_note, tile_path,
    verify_checkpoint, verify_consistency, verify_inclusion, verify_note,
};
pub use types::{
    Attestation, Benchmark, Candidate, Error, Evidence, MeshContractQuery, MeshQueryOut, NameRef,
    ProofRecord, Provenance, PublishOut, RejectedNode, Result, SpecRecord, Submission,
    type_skeleton,
};
