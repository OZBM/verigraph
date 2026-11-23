//! Failing slices, counterexample diffs, docs/15 `FailureReport`, and runtime guard reports.

pub mod diff;
pub mod guard;
pub mod names;
pub mod report;
pub mod slice;
pub mod trace;

pub use diff::diff;
pub use guard::{GuardEvent, GuardMap, GuardMapEntry, guard_report, parse_guard_event};
pub use names::Names;
pub use report::{FailureReport, OracleEntry, Reproduced, SliceEntry, prose, report};
pub use slice::slice;
pub use trace::ObligationKind;
