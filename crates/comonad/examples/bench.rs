//! Throughput of the zipper, store and lens at chat-history scale. Run with
//! `cargo run --release --example bench`; `bench <name>` runs one case, for
//! profiling.
use control_comonad::{NodeId, Store, Tree, cut};
use std::hint::black_box;
use std::time::Instant;

const N: usize = 1_000_000;

/// A deterministic conversation-shaped tree: mostly long chains (turns), with
/// occasional forks (spawned agents) back to a recent node.
fn conversation(n: usize) -> (Tree<u64>, Vec<NodeId>) {
    let mut tree = Tree::with_capacity(n);
    let mut ids = Vec::with_capacity(n);
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    for i in 0..n {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let parent = match i {
            0 => None,
            _ if seed.is_multiple_of(64) => Some(ids[(seed as usize >> 8) % i]),
            _ if seed % 4096 == 1 => None,
            _ => Some(ids[i - 1]),
        };
        ids.push(tree.push(parent, i as u64));
    }
    (tree, ids)
}

fn time(name: &str, ops: usize, only: Option<&str>, f: impl FnOnce() -> u64) {
    if only.is_some_and(|o| o != name) {
        return;
    }
    let start = Instant::now();
    let check = black_box(f());
    let ns = start.elapsed().as_nanos() as f64 / ops as f64;
    println!("{name:<28} {ns:>10.2} ns/op   ({ops} ops, check {check})");
}

fn main() {
    let only = std::env::args().nth(1);
    let only = only.as_deref();
    time("tree_push", N, only, || conversation(N).0.len() as u64);
    let (tree, ids) = conversation(N);
    time("focus_root", N, only, || ids.iter().map(|id| tree.focus(*id).unwrap().root().id().index() as u64).sum());
    time("ancestors_depth", N, only, || ids.iter().step_by(97).map(|id| tree.focus(*id).unwrap().depth() as u64).sum::<u64>() * 97);
    time("subtree_walk", N, only, || {
        let mut roots = 0u64;
        let mut seen = 0u64;
        for id in &ids {
            let f = tree.focus(*id).unwrap();
            if f.up().is_none() {
                roots += 1;
                seen += f.subtree().count() as u64;
            }
        }
        seen + roots
    });
    time("fold_subtree_sizes", N, only, || *tree.fold(|_, kids| 1 + kids.copied().sum::<u64>()).first().unwrap());
    let timeline: Vec<u64> = (0..N as u64).collect();
    time("store_extend_window", N, only, || {
        let w = Store::new(&timeline[..], 0).unwrap();
        w.extend(|s| s.window(2).iter().sum::<u64>()).into_iter().fold(0, u64::wrapping_add)
    });
    time("lens_cut_1M", N / 10, only, || (0..N as u64 / 10).map(|i| cut(N as u64 + i, 32).count() as u64).sum());
}
