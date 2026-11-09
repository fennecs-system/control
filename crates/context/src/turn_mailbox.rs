//! Attempt-scoped deliveries and drafts. Pure transitions; persistence and
//! authority checks belong to the host. Reading never advances processing.
use crate::working_state::Payload;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_DELIVERIES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum Range {
    Room {
        room: String,
        after: u64,
        through: u64,
    },
    Watch {
        watch: String,
        after: u64,
        through: u64,
    },
    Conversation {
        chat: String,
        after: u64,
        through: u64,
    },
}
impl Range {
    fn parts(&self) -> (&str, u64, u64) {
        match self {
            Self::Room {
                room,
                after,
                through,
            } => (room, *after, *through),
            Self::Watch {
                watch,
                after,
                through,
            } => (watch, *after, *through),
            Self::Conversation {
                chat,
                after,
                through,
            } => (chat, *after, *through),
        }
    }
    fn source(&self) -> (&str, &str) {
        let (id, _, _) = self.parts();
        (
            match self {
                Self::Room { .. } => "room",
                Self::Watch { .. } => "watch",
                Self::Conversation { .. } => "conversation",
            },
            id,
        )
    }
    fn through_mut(&mut self) -> &mut u64 {
        match self {
            Self::Room { through, .. }
            | Self::Watch { through, .. }
            | Self::Conversation { through, .. } => through,
        }
    }
    pub fn through(&self) -> u64 {
        self.parts().2
    }
    fn validate(&self) -> Result<(), &'static str> {
        let (id, after, through) = self.parts();
        if id.is_empty()
            || id.len() > 128
            || id.bytes().any(|b| b.is_ascii_control())
            || after > through
        {
            return Err("Invalid delivery source or range");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    pub ranges: Vec<Range>,
    /// Missing/rotated or oversized data must be explicitly resynchronised.
    pub gap: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub request: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Disposition {
    Continue,
    Wait {
        topics: Vec<String>,
        #[serde(default)]
        recheck_seconds: Option<u32>,
    },
    Done,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    pub base_revision: u64,
    pub payload: Payload,
    pub processed: Vec<String>,
    #[serde(default)]
    pub resynchronised: Vec<String>,
    pub disposition: Disposition,
    pub progress: Option<String>,
}
impl Handoff {
    fn validate(&self) -> Result<(), &'static str> {
        self.payload.validate()?;
        if self.processed.len() > MAX_DELIVERIES || self.resynchronised.len() > MAX_DELIVERIES {
            return Err("Too many delivery receipts");
        }
        if self
            .progress
            .as_ref()
            .is_some_and(|p| p.chars().count() > 1000)
        {
            return Err("Progress must contain at most 1000 characters");
        }
        if let Disposition::Wait {
            topics,
            recheck_seconds,
        } = &self.disposition
            && (topics.len() > 16
                || recheck_seconds.is_some_and(|seconds| seconds == 0)
                || topics.iter().any(|t| t.is_empty() || t.len() > 128))
        {
            return Err("Wait requires at most 16 existing topics and an optional positive recheck");
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mailbox {
    /// Ephemeral hint watermark, distinct from delivery and processing.
    #[serde(skip)]
    pub announced: Option<(usize, usize)>,
    /// Host-selected starting watermarks. Captured before provider launch;
    /// models cannot insert baselines through handoff arguments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub baseline: Vec<Range>,
    /// Opaque host-issued keys are scoped by the owning attempt generation.
    pub deliveries: BTreeMap<String, Delivery>,
    pub drafts: BTreeMap<String, Handoff>,
    pub accepted: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkingState {
    pub revision: u64,
    pub run: String,
    pub generation: String,
    pub checkpoint: String,
    pub payload: Payload,
    pub processed: Vec<Range>,
    pub disposition: Disposition,
}
impl Mailbox {
    pub fn deliver(&mut self, key: String, delivery: Delivery) -> Result<(), &'static str> {
        if let Some(previous) = self.deliveries.get(&key) {
            return if previous == &delivery {
                Ok(())
            } else {
                Err("Delivery identity reused with different contents")
            };
        }
        if key.is_empty()
            || key.len() > 128
            || delivery.ranges.len() > 16
            || self.deliveries.len() >= MAX_DELIVERIES
        {
            return Err("Delivery budget exceeded or invalid identity");
        }
        for range in &delivery.ranges {
            range.validate()?;
        }
        self.deliveries.insert(key, delivery);
        Ok(())
    }
    /// Saving the returned state is the host's responsibility. A rejected draft
    /// never changes the accepted draft or processes any delivered evidence.
    pub fn stage(
        &mut self,
        key: String,
        handoff: Handoff,
        revision: u64,
    ) -> Result<(), &'static str> {
        if let Some(previous) = self.drafts.get(&key) {
            return if previous == &handoff {
                Ok(())
            } else {
                Err("Handoff identity reused with different contents")
            };
        }
        if key.is_empty() || key.len() > 128 || self.drafts.len() >= 8 {
            return Err("Handoff correction budget exceeded or invalid identity");
        }
        handoff.validate()?;
        if handoff.base_revision != revision {
            return Err("Working state revision changed");
        }
        for receipt in &handoff.processed {
            let delivery = self
                .deliveries
                .get(receipt)
                .ok_or("Receipt was not delivered to this attempt")?;
            if delivery.gap && !handoff.resynchronised.contains(receipt) {
                return Err("Delivery gap requires explicit resynchronisation");
            }
        }
        if handoff
            .resynchronised
            .iter()
            .any(|r| !handoff.processed.contains(r))
        {
            return Err("Resynchronisation requires a processed delivery receipt");
        }
        self.accepted = Some(key.clone());
        self.drafts.insert(key, handoff);
        Ok(())
    }
    pub fn accepted(&self) -> Option<&Handoff> {
        self.accepted.as_ref().and_then(|key| self.drafts.get(key))
    }
    /// Construct a publication candidate, without changing committed state.
    /// Exact contiguous ranges may advance; unseen gaps cannot be skipped by
    /// acknowledging a later page. The host commits this with its workspace.
    pub fn publication(
        &self,
        previous: Option<&WorkingState>,
        run: &str,
        generation: &str,
        checkpoint: &str,
    ) -> Result<Option<WorkingState>, &'static str> {
        let Some(handoff) = self.accepted() else {
            return Ok(None);
        };
        let revision = previous.map_or(0, |p| p.revision);
        if revision != handoff.base_revision {
            return Err("Working state revision changed before publication");
        }
        let mut processed = previous.map_or_else(Vec::new, |p| p.processed.clone());
        for range in &self.baseline {
            range.validate()?;
            if let Some(existing) = processed.iter_mut().find(|r| r.source() == range.source()) {
                // A later admission captures a newer direct request. Its
                // host-owned starting watermark must not regress to the
                // previous session's baseline when callbacks are committed.
                *existing.through_mut() = existing.through().max(range.through());
            } else {
                processed.push(range.clone());
            }
        }
        let mut pending = Vec::new();
        for key in &handoff.processed {
            let delivery = self
                .deliveries
                .get(key)
                .ok_or("Retained handoff refers to an unavailable receipt")?;
            pending.extend(
                delivery
                    .ranges
                    .iter()
                    .map(|range| (range, delivery.gap && handoff.resynchronised.contains(key))),
            );
        }
        pending.sort_by_key(|(range, _)| range.parts().1);
        for (range, resynchronised) in pending {
            let (_, after, through) = range.parts();
            if let Some(existing) = processed.iter_mut().find(|r| r.source() == range.source()) {
                if after > existing.through() && !resynchronised {
                    return Err("Cannot process an unseen event range");
                }
                *existing.through_mut() = existing.through().max(through);
            } else {
                if after != 0 && !resynchronised {
                    return Err(
                        "First event range requires an explicit baseline or resynchronisation",
                    );
                }
                processed.push(range.clone());
            }
        }
        if processed.len() > 128 {
            return Err("Processed source budget exceeded; retire obsolete sources explicitly");
        }
        Ok(Some(WorkingState {
            revision: revision
                .checked_add(1)
                .ok_or("Working state revision exhausted")?,
            run: run.into(),
            generation: generation.into(),
            checkpoint: checkpoint.into(),
            payload: handoff.payload.clone(),
            processed,
            disposition: handoff.disposition.clone(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn handoff(processed: &[&str]) -> Handoff {
        Handoff {
            base_revision: 0,
            payload: Payload {
                subgoal: "Keep making progress".into(),
                ..Default::default()
            },
            processed: processed.iter().map(|s| (*s).into()).collect(),
            resynchronised: vec![],
            disposition: Disposition::Continue,
            progress: None,
        }
    }
    fn page(after: u64, through: u64) -> Delivery {
        Delivery {
            ranges: vec![Range::Room {
                room: "general".into(),
                after,
                through,
            }],
            gap: false,
            request: String::new(),
        }
    }
    #[test]
    fn empty_source_positions_cannot_acknowledge_unseen_data() {
        let mut mailbox = Mailbox::default();
        mailbox.deliver("empty".into(), page(0, 0)).unwrap();
        mailbox.stage("draft".into(), handoff(&["empty"]), 0).unwrap();
        let state = mailbox.publication(None, "r", "g", "cp").unwrap().unwrap();
        assert_eq!(state.processed[0].through(), 0);
        let mut unseen = Mailbox::default();
        unseen.deliver("empty".into(), page(10, 10)).unwrap();
        unseen.stage("draft".into(), handoff(&["empty"]), 0).unwrap();
        assert!(unseen.publication(None, "r", "g", "cp").is_err());
    }
    #[test]
    fn a_new_admission_baseline_can_follow_an_older_committed_conversation_cursor() {
        let mut first = Mailbox::default();
        first.baseline.push(Range::Conversation {
            chat: "c-1".into(),
            after: 0,
            through: 5,
        });
        first.stage("first".into(), handoff(&[]), 0).unwrap();
        let previous = first.publication(None, "r1", "g1", "cp1").unwrap().unwrap();
        let mut next = Mailbox::default();
        next.baseline.push(Range::Conversation {
            chat: "c-1".into(),
            after: 0,
            through: 8,
        });
        next.deliver(
            "new".into(),
            Delivery {
                ranges: vec![Range::Conversation {
                    chat: "c-1".into(),
                    after: 8,
                    through: 9,
                }],
                gap: false,
                request: String::new(),
            },
        )
        .unwrap();
        let mut candidate = handoff(&["new"]);
        candidate.base_revision = 1;
        next.stage("next".into(), candidate, 1).unwrap();
        let published = next
            .publication(Some(&previous), "r2", "g2", "cp2")
            .unwrap()
            .unwrap();
        assert_eq!(published.processed[0].through(), 9);
    }
    #[test]
    fn read_is_not_processing_and_failed_staging_leaves_no_acknowledgement() {
        let mut mailbox = Mailbox::default();
        mailbox.deliver("page".into(), page(0, 8)).unwrap();
        assert_eq!(
            mailbox
                .publication(None, "run", "attempt", "checkpoint")
                .unwrap(),
            None
        );
        let before = mailbox.clone();
        assert!(
            mailbox
                .stage("handoff".into(), handoff(&["unseen"]), 0)
                .is_err()
        );
        assert_eq!(mailbox, before);
    }
    #[test]
    fn out_of_order_pages_commit_contiguously_and_newer_arrivals_remain_unprocessed() {
        let mut mailbox = Mailbox::default();
        mailbox.deliver("first".into(), page(0, 8)).unwrap();
        mailbox.deliver("second".into(), page(8, 16)).unwrap();
        mailbox
            .stage("handoff".into(), handoff(&["second", "first"]), 0)
            .unwrap();
        mailbox.deliver("racing".into(), page(16, 17)).unwrap();
        let committed = mailbox
            .publication(None, "run", "attempt", "checkpoint")
            .unwrap()
            .unwrap();
        assert_eq!(committed.processed[0].through(), 16);
        assert_eq!(committed.revision, 1);
    }
    #[test]
    fn gaps_stale_revisions_and_conflicting_replays_are_rejected() {
        let mut mailbox = Mailbox::default();
        mailbox.deliver("later".into(), page(8, 16)).unwrap();
        mailbox.stage("h".into(), handoff(&["later"]), 0).unwrap();
        assert!(
            mailbox
                .publication(None, "run", "attempt", "checkpoint")
                .is_err()
        );
        let before = mailbox.clone();
        assert!(mailbox.stage("h".into(), handoff(&[]), 0).is_err());
        assert_eq!(mailbox, before);
        assert!(mailbox.stage("stale".into(), handoff(&[]), 1).is_err());
        let mut gap = page(0, 8);
        gap.gap = true;
        mailbox.deliver("gap".into(), gap).unwrap();
        assert!(mailbox.stage("gap-h".into(), handoff(&["gap"]), 0).is_err());
        let mut corrected = handoff(&["gap"]);
        corrected.resynchronised.push("gap".into());
        mailbox.stage("resync".into(), corrected, 0).unwrap();
        assert!(
            mailbox
                .publication(None, "run", "attempt", "checkpoint")
                .unwrap()
                .is_some()
        );
    }
    #[test]
    fn same_payload_replays_do_not_revert_a_newer_correction() {
        let mut mailbox = Mailbox::default();
        mailbox.stage("first".into(), handoff(&[]), 0).unwrap();
        let mut correction = handoff(&[]);
        correction.disposition = Disposition::Done;
        mailbox
            .stage("second".into(), correction.clone(), 0)
            .unwrap();
        mailbox.stage("first".into(), handoff(&[]), 0).unwrap();
        assert_eq!(mailbox.accepted(), Some(&correction));
    }
}
