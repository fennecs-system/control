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
        let start = self.start & !((1u64 << level) - 1);
        start.checked_add(1 << level).map(|_| Block { start, level })
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
mod tests;
