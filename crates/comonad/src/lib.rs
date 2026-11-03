//! Comonadic views of Control's histories: a focus and everything around it.
//!
//! A history is a [`Space`] of positions. A [`Store`] focuses one position and
//! can look anywhere from there; [`Tree`] gives chat trees O(1) zipper moves;
//! [`lens`] cuts an unbounded timeline into a bounded view that is verbatim at
//! the focus and coarser with distance, in blocks that never change once
//! complete. Pure and std-only: storage, summaries and I/O stay with callers.
pub mod lens;
pub mod store;
pub mod tree;

pub use lens::{Block, cut};
pub use store::{Space, Store};
pub use tree::{Focus, NodeId, Tree};
