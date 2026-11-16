//! Attempt-scoped deliveries and drafts. Pure transitions; persistence and
//! authority checks belong to the host. Reading never advances processing.
use crate::working_state::Payload;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_DELIVERIES: usize = 128;
/// Committed watermarks per working state. Sources the host reports as retired
/// are dropped at publication and never count against this budget.
pub const MAX_SOURCES: usize = 128;
/// A receipt left pending by [`Mailbox::applicable`], with the reason.
pub type Dropped = (String, &'static str);

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
            return Err(
                "Invalid delivery source or range: the host issued a malformed receipt; ask the operator via control_design_ask",
            );
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
            return Err(
                "Too many delivery receipts: retry control_turn_handoff {\"disposition\":{\"kind\":\"continue\"}} without processed, so the host applies every receipt it delivered",
            );
        }
        if self
            .progress
            .as_ref()
            .is_some_and(|p| p.chars().count() > 1000)
        {
            return Err(
                "Progress must contain at most 1000 characters: retry control_turn_handoff {\"disposition\":{\"kind\":\"continue\"}} with a shorter progress, or omit it",
            );
        }
        if let Disposition::Wait {
            topics,
            recheck_seconds,
        } = &self.disposition
            && (topics.len() > 16
                || recheck_seconds.is_some_and(|seconds| seconds == 0)
                || topics.iter().any(|t| t.is_empty() || t.len() > 128))
        {
            return Err(
                "Wait requires at most 16 existing topics and an optional positive recheck: retry control_turn_handoff {\"disposition\":{\"kind\":\"wait\",\"topics\":[]}} or name topics from wake_topics",
            );
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
                Err(
                    "Delivery identity reused with different contents: the host reused a receipt key; ask the operator via control_design_ask",
                )
            };
        }
        if key.is_empty()
            || key.len() > 128
            || delivery.ranges.len() > 16
            || self.deliveries.len() >= MAX_DELIVERIES
        {
            return Err(
                "Delivery budget exceeded or invalid identity: this turn read its maximum of 128 deliveries; hand off with control_turn_handoff {\"disposition\":{\"kind\":\"continue\"}} and read the rest next turn",
            );
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
                Err(
                    "Handoff identity reused with different contents: retry control_turn_handoff {\"disposition\":{\"kind\":\"continue\"}} as a new call",
                )
            };
        }
        if key.is_empty() || key.len() > 128 {
            return Err(
                "Invalid handoff identity: retry control_turn_handoff {\"disposition\":{\"kind\":\"continue\"}} as a new call, or ask the operator via control_design_ask",
            );
        }
        handoff.validate()?;
        if handoff.base_revision != revision {
            return Err(
                "Working state revision changed: retry control_turn_handoff {\"disposition\":{\"kind\":\"continue\"}} without base_revision so the host uses the committed revision",
            );
        }
        for receipt in &handoff.processed {
            let delivery = self.deliveries.get(receipt).ok_or(
                "Receipt was not delivered to this attempt: retry control_turn_handoff {\"disposition\":{\"kind\":\"continue\"}} without processed, so the host applies the receipts it delivered",
            )?;
            if delivery.gap && !handoff.resynchronised.contains(receipt) {
                return Err(
                    "Delivery gap requires explicit resynchronisation: inspect the source, then retry control_turn_handoff {\"processed\":[\"<receipt>\"],\"resynchronised\":[\"<receipt>\"],\"disposition\":{\"kind\":\"continue\"}}",
                );
            }
        }
        if handoff
            .resynchronised
            .iter()
            .any(|r| !handoff.processed.contains(r))
        {
            return Err(
                "Resynchronisation requires a processed delivery receipt: list each resynchronised receipt in processed too: retry control_turn_handoff {\"processed\":[\"<receipt>\"],\"resynchronised\":[\"<receipt>\"],\"disposition\":{\"kind\":\"continue\"}}",
            );
        }
        // Only the accepted draft is ever published; superseded corrections
        // are evidence of nothing, so they make room instead of exhausting it.
        if self.drafts.len() >= 8 {
            let accepted = self.accepted.clone();
            self.drafts.retain(|k, _| Some(k) == accepted.as_ref());
        }
        self.accepted = Some(key.clone());
        self.drafts.insert(key, handoff);
        Ok(())
    }
    pub fn accepted(&self) -> Option<&Handoff> {
        self.accepted.as_ref().and_then(|key| self.drafts.get(key))
    }
    /// Committed watermarks merged with this attempt's host-owned baseline.
    fn watermarks(&self, previous: Option<&WorkingState>) -> Result<Vec<Range>, &'static str> {
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
        Ok(processed)
    }
    /// Advances `processed` through the receipts' ranges, oldest first. An
    /// error names the first receipt that cannot be applied contiguously.
    fn advance<'a>(
        &self,
        processed: &mut Vec<Range>,
        receipts: &'a [String],
        resynchronised: &[String],
    ) -> Result<(), (&'static str, &'a str)> {
        let mut pending = Vec::new();
        for key in receipts {
            let delivery = self.deliveries.get(key).ok_or((
                "Retained handoff refers to an unavailable receipt: retry control_turn_handoff without processed",
                key.as_str(),
            ))?;
            pending.extend(delivery.ranges.iter().map(|range| {
                (range, delivery.gap && resynchronised.contains(key), key.as_str())
            }));
        }
        pending.sort_by_key(|(range, _, _)| range.parts().1);
        for (range, resynchronised, key) in pending {
            let (_, after, through) = range.parts();
            if let Some(existing) = processed.iter_mut().find(|r| r.source() == range.source()) {
                if after > existing.through() && !resynchronised {
                    return Err((
                        "Cannot process an unseen event range. Retry control_turn_handoff without processed, so the host applies only receipts that follow the committed position",
                        key,
                    ));
                }
                *existing.through_mut() = existing.through().max(through);
            } else {
                if after != 0 && !resynchronised {
                    return Err((
                        "First event range requires an explicit baseline or resynchronisation. Retry control_turn_handoff without processed, so the host applies only receipts that follow the committed position",
                        key,
                    ));
                }
                processed.push(range.clone());
            }
        }
        Ok(())
    }
    /// Splits `receipts` into those that can be processed from the committed
    /// position and those that cannot, with the reason. Dropping a receipt
    /// only leaves its events pending, so a host may apply the kept ones
    /// instead of rejecting a handoff. Unknown receipts, unresolved gaps and
    /// ranges that would skip unseen events are never kept.
    pub fn applicable(
        &self,
        previous: Option<&WorkingState>,
        receipts: &[String],
        resynchronised: &[String],
    ) -> Result<(Vec<String>, Vec<Dropped>), &'static str> {
        let mut kept = Vec::new();
        let mut dropped = Vec::new();
        for receipt in receipts {
            if kept.contains(receipt) || dropped.iter().any(|(r, _)| r == receipt) {
                continue;
            }
            match self.deliveries.get(receipt) {
                None => dropped.push((receipt.clone(), "not delivered to this attempt")),
                Some(d) if d.gap && !resynchronised.contains(receipt) => {
                    dropped.push((receipt.clone(), "has a gap and was not marked resynchronised"))
                }
                Some(_) => kept.push(receipt.clone()),
            }
        }
        let base = self.watermarks(previous)?;
        loop {
            let mut processed = base.clone();
            match self.advance(&mut processed, &kept, resynchronised) {
                Ok(()) => return Ok((kept, dropped)),
                Err((_, key)) => {
                    let key = key.to_owned();
                    kept.retain(|k| *k != key);
                    dropped.push((key, "starts after an unprocessed earlier range of its source"));
                }
            }
        }
    }
    /// Construct a publication candidate, without changing committed state.
    /// Exact contiguous ranges may advance; unseen gaps cannot be skipped by
    /// acknowledging a later page. The host commits this with its workspace.
    ///
    /// `live` reports whether a source can still deliver events. A watermark
    /// is only needed to prevent redelivery, so the host may report a source
    /// retired only once nothing can read from it again (for example a watch
    /// record it has deleted). Retired watermarks are dropped after this
    /// publication's ranges are checked, so a contiguous receipt for a source
    /// retired mid-attempt is still accepted rather than stranding the bot.
    pub fn publication(
        &self,
        previous: Option<&WorkingState>,
        run: &str,
        generation: &str,
        checkpoint: &str,
        live: impl Fn(&Range) -> bool,
    ) -> Result<Option<WorkingState>, &'static str> {
        let Some(handoff) = self.accepted() else {
            return Ok(None);
        };
        let revision = previous.map_or(0, |p| p.revision);
        if revision != handoff.base_revision {
            return Err(
                "Working state revision changed before publication: call control_turn_context {} for the committed notes, then retry control_turn_handoff without base_revision",
            );
        }
        let mut processed = self.watermarks(previous)?;
        self.advance(&mut processed, &handoff.processed, &handoff.resynchronised)
            .map_err(|(message, _)| message)?;
        processed.retain(|range| live(range));
        if processed.len() > MAX_SOURCES {
            return Err(
                "More than 128 live callback sources (rooms, watches, conversations) would be tracked. Sources of deleted watches are retired automatically; stop watches you no longer need with control_watch_stop {\"watch_id\":\"<id>\"}, or ask the operator via control_design_ask",
            );
        }
        Ok(Some(WorkingState {
            revision: revision.checked_add(1).ok_or(
                "Working state revision exhausted: ask the operator via control_design_ask",
            )?,
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
mod tests;
