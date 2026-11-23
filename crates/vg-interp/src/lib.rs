//! Reference interpreter: the executable semantics and the test oracle.

pub mod bigint;
pub mod bytes;
pub mod eval;
pub mod float_ops;
pub mod host;
pub mod json;

pub use eval::run;
pub use host::{
    Host, NoEffectsHost, RecordingHost, TraceEntry, TraceHost, Trap, parse_trace_json,
    trace_to_json,
};
pub use json::value_from_json;
