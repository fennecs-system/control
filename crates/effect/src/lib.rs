//! Events in, effects out. `machine` is pure: no clocks, I/O or locks. `sync`
//! holds the blocking primitives drivers use to wait without polling; their
//! shape follows tokio's so a later async driver replaces drivers, not machines.
pub mod machine;
pub mod sync;
