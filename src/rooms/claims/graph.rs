//! The claim dependency graph: blocked claims point at the claim key they need
//! (`blocked_on`), so each claim has at most one outgoing edge. The advisor's
//! bottleneck facts and the `control_room_plan` tool walk it the same way.
use super::{Claim, ClaimState};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// Word-at-a-time multiplicative hash (rustc's Fx) for per-pass maps over local
/// chat IDs and claim keys. SipHash lookups dominated the measured advisor
/// scan; these maps live for one pass and never key on remote input.
#[derive(Default)]
pub(crate) struct Fx(u64);
impl Fx {
    pub(crate) fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}
impl Hasher for Fx {
    fn write(&mut self, bytes: &[u8]) {
        let (words, rest) = bytes.as_chunks::<8>();
        for word in words {
            self.add(u64::from_le_bytes(*word));
        }
        let mut tail = [0; 8];
        tail[..rest.len()].copy_from_slice(rest);
        self.add(u64::from_le_bytes(tail));
    }
    fn write_u8(&mut self, byte: u8) {
        self.add(u64::from(byte));
    }
    fn finish(&self) -> u64 {
        self.0
    }
}
pub(crate) type Fast<K, V> = HashMap<K, V, BuildHasherDefault<Fx>>;

/// Live waiters by the (room, key) they are blocked on: blocked claims whose
/// owner has not been lost.
#[derive(Default)]
pub(crate) struct Waits<'a>(Fast<(&'a str, &'a str), Vec<&'a Claim>>);
/// Everything transitively waiting on one claim key.
pub(crate) struct Reach<'a> {
    /// Dependent claims, breadth first so the nearest come first.
    pub dependents: Vec<&'a Claim>,
    /// Distinct dependent owners other than the producer's owner, sorted.
    pub members: Vec<&'a str>,
    /// The producer waits on itself through its dependents.
    pub cycle: bool,
}
impl<'a> Waits<'a> {
    pub(crate) fn of(claims: impl IntoIterator<Item = &'a Claim>) -> Self {
        let mut waits = Self::default();
        for claim in claims {
            if let ClaimState::Blocked { on: Some(on) } = claim.state() {
                waits.0.entry((claim.room.as_str(), on)).or_default().push(claim);
            }
        }
        waits
    }
    /// Number of live waiting claims.
    pub(crate) fn edges(&self) -> usize {
        self.0.values().map(Vec::len).sum()
    }
    pub(crate) fn waited(&self, room: &str, key: &str) -> bool {
        self.0.contains_key(&(room, key))
    }
    /// Keys someone waits on, in no particular order.
    pub(crate) fn keys(&self) -> impl Iterator<Item = (&'a str, &'a str)> + '_ {
        self.0.keys().copied()
    }
    /// Breadth-first walk up `blocked_on` edges from `key`. The visited set
    /// makes cycles terminate; `owner` (the producer's) is not a dependent member.
    pub(crate) fn reach(&self, room: &'a str, key: &'a str, owner: Option<&str>) -> Reach<'a> {
        let mut visited: Fast<&str, ()> = Fast::default();
        let mut cycle = false;
        let mut dependents: Vec<&Claim> = Vec::new();
        let mut next = 0;
        let mut at = Some((room, key));
        while let Some((room, at_key)) = at {
            for &claim in self.0.get(&(room, at_key)).into_iter().flatten() {
                if claim.key == key {
                    cycle = true;
                } else if visited.insert(claim.key.as_str(), ()).is_none() {
                    dependents.push(claim);
                }
            }
            at = dependents
                .get(next)
                .map(|claim| (claim.room.as_str(), claim.key.as_str()));
            next += 1;
        }
        let mut members = dependents
            .iter()
            .map(|claim| claim.owner.as_str())
            .filter(|member| Some(*member) != owner)
            .collect::<Vec<_>>();
        members.sort_unstable();
        members.dedup();
        Reach {
            dependents,
            members,
            cycle,
        }
    }
}
