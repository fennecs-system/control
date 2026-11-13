
use super::*;

fn check(len: u64, verbatim: u64) -> Vec<Block> {
    let blocks: Vec<Block> = cut(len, verbatim).collect();
    let mut end = len;
    for b in &blocks {
        assert_eq!(
            b.end(),
            end,
            "contiguous, newest first: {len}/{verbatim} {blocks:?}"
        );
        assert_eq!(b.start % b.len(), 0, "aligned");
        end = b.start;
    }
    assert_eq!(end, 0, "covers all history");
    let tail = verbatim.min(len) as usize;
    assert!(
        blocks[..tail].iter().all(Block::is_item),
        "the newest items are verbatim"
    );
    if len > 0 {
        let log = 64 - len.leading_zeros() as usize;
        assert!(
            blocks.len() <= tail + 2 * log,
            "O(log n): {} blocks for {len}",
            blocks.len()
        );
    }
    blocks
}

#[test]
fn cuts_cover_history_with_aligned_logarithmic_blocks() {
    for len in 0..600 {
        for verbatim in [0, 1, 5, 40] {
            check(len, verbatim);
        }
    }
    for len in [1 << 20, (1 << 32) + 12345, u64::MAX / 3] {
        check(len, 16);
    }
    assert!(check(1_000_000, 0).len() <= 40);
}

#[test]
fn resolution_falls_with_distance_from_the_focus() {
    let blocks = check(10_000, 8);
    let levels: Vec<u32> = blocks.iter().map(|b| b.level).collect();
    assert!(
        levels.windows(2).all(|w| w[1] + 1 >= w[0]),
        "never coarser near the focus than farther away, beyond one step: {levels:?}"
    );
    assert!(levels.first() == Some(&0) && levels.last() > Some(&8));
}

#[test]
fn growing_history_computes_each_summary_once() {
    // Blocks are fixed ranges, so a summary is never invalidated: across every
    // cut while history grows, the distinct summarised blocks stay below the
    // number of items, i.e. O(1) amortised summaries per message.
    let n = 4096;
    let mut summaries = std::collections::HashSet::new();
    for len in 1..=n {
        summaries.extend(cut(len, 16).filter(|b| !b.is_item()));
    }
    assert!(
        summaries.len() as u64 <= n,
        "{} summaries for {n} items",
        summaries.len()
    );
    let b = Block {
        start: 512,
        level: 7,
    };
    let [l, r] = b.children().unwrap();
    assert_eq!(
        (l.parent(), r.parent(), l.end(), r.start),
        (Some(b), Some(b), r.start, 576)
    );
    assert!(Block { start: 3, level: 0 }.children().is_none());
    assert!(
        cut(u64::MAX, 0).next().unwrap().parent().is_none(),
        "no parent past the end of u64"
    );
}
