//! A bounded view of an unbounded timeline, focused on its end.
//!
//! The newest `verbatim` items stay whole; older history is covered by aligned
//! power-of-two blocks that grow at most one level per step away from the
//! focus, so any history needs at most two blocks per level: O(log n). A block
//! covers fixed positions, so its contents (and any summary of them) never
//! change once complete; growing the timeline only regroups blocks near the
//! end and occasionally merges two old blocks into their parent. Blocks form
//! an implicit binary tree: a summary of a block is a summary of its two
//! children, and zooming in reaches the verbatim items.

/// Positions `start .. start + 2^level`. Always aligned: `start % 2^level == 0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Block {
    pub start: u64,
    pub level: u32,
}
impl Block {
    #[inline]
    pub fn len(&self) -> u64 {
        1 << self.level
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        false
    }
    #[inline]
    pub fn end(&self) -> u64 {
        self.start + self.len()
    }
    /// A single item, shown verbatim rather than summarised.
    #[inline]
    pub fn is_item(&self) -> bool {
        self.level == 0
    }
    /// The two halves a summary of this block is made from.
    #[inline]
    pub fn children(&self) -> Option<[Block; 2]> {
        let level = self.level.checked_sub(1)?;
        Some([Block { start: self.start, level }, Block { start: self.start + (1 << level), level }])
    }
    /// The block this one is a half of.
    #[inline]
    pub fn parent(&self) -> Option<Block> {
        let level = self.level.checked_add(1).filter(|l| *l < 64)?;
        Some(Block { start: self.start & !((1u64 << level) - 1), level })
    }
}

/// The cut of `0..len`, newest block first: `verbatim` single items, then
/// aligned blocks growing toward the start of history.
pub fn cut(len: u64, verbatim: u64) -> Cut {
    Cut { end: len, verbatim, level: 0 }
}

#[derive(Clone, Debug)]
pub struct Cut {
    end: u64,
    verbatim: u64,
    level: u32,
}
impl Iterator for Cut {
    type Item = Block;
    #[inline]
    fn next(&mut self) -> Option<Block> {
        if self.end == 0 {
            return None;
        }
        let level = if self.verbatim > 0 {
            self.verbatim -= 1;
            0
        } else {
            // Grow one level when the boundary is aligned for it, else stay:
            // taking a block of the current level always aligns the next one.
            let up = self.level + 1;
            let fits = |level: u32| level < 64 && self.end.trailing_zeros() >= level && (1u64 << level) <= self.end;
            if fits(up) { up } else if fits(self.level) { self.level } else { self.end.trailing_zeros().min(63 - self.end.leading_zeros()) }
        };
        self.level = level;
        self.end -= 1 << level;
        Some(Block { start: self.end, level })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(len: u64, verbatim: u64) -> Vec<Block> {
        let blocks: Vec<Block> = cut(len, verbatim).collect();
        let mut end = len;
        for b in &blocks {
            assert_eq!(b.end(), end, "contiguous, newest first: {len}/{verbatim} {blocks:?}");
            assert_eq!(b.start % b.len(), 0, "aligned");
            end = b.start;
        }
        assert_eq!(end, 0, "covers all history");
        let tail = verbatim.min(len) as usize;
        assert!(blocks[..tail].iter().all(Block::is_item), "the newest items are verbatim");
        if len > 0 {
            let log = 64 - len.leading_zeros() as usize;
            assert!(blocks.len() <= tail + 2 * log, "O(log n): {} blocks for {len}", blocks.len());
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
        assert!(levels.windows(2).all(|w| w[1] + 1 >= w[0]), "never coarser near the focus than farther away, beyond one step: {levels:?}");
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
        assert!(summaries.len() as u64 <= n, "{} summaries for {n} items", summaries.len());
        let b = Block { start: 512, level: 7 };
        let [l, r] = b.children().unwrap();
        assert_eq!((l.parent(), r.parent(), l.end(), r.start), (Some(b), Some(b), r.start, 576));
        assert!(Block { start: 3, level: 0 }.children().is_none());
    }
}
