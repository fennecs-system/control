//! Borrowed one-shot room callback admission. The host supplies authorization
//! and retains exclusive ownership while interpreting the resulting plans.
use super::{conversations::Session, runs::Run};
use crate::{
    identity::{Action, Actor},
    rooms::Post,
};
use control_context::agent_policy::RunKind;
use std::collections::{HashMap, HashSet};

const WINDOW: usize = 512;

pub(crate) struct View<'a> {
    pub runs: &'a [Run],
    pub chats: &'a [Session],
    pub posts: &'a [Post],
}

/// Captured subscription input; contains no mutable watch or global-state handle.
pub(crate) struct WakeRef<'a> {
    pub chat: &'a str,
    pub actor: &'a Actor,
    pub room: &'a str,
    pub cursor: usize,
    pub origin: &'a str,
    pub generation: &'a str,
    pub scanned: usize,
}

pub(crate) fn normal_task(run: &Run) -> bool {
    run.persona.is_none()
        && run.continuation.is_none()
        && matches!(
            run.kind,
            RunKind::Chat | RunKind::SelfImprovement | RunKind::Steering
        )
        && run.source != "room-notification"
}

/// Follow only durable continuation receipts, never shared-root guesses or
/// independent spawned children. Every edge retains the parent's scope.
pub(crate) fn continuation_leaf(
    history: &[Run],
    origin: usize,
    runs: &HashMap<&str, usize>,
) -> Option<usize> {
    if history[origin].status != "continued" {
        return Some(origin);
    }
    let mut index = origin;
    let mut seen = Vec::with_capacity(8);
    for _ in 0..64 {
        let run = &history[index];
        if seen.contains(&index) {
            return None;
        }
        seen.push(index);
        if run.status != "continued" {
            return Some(index);
        }
        let generation = run.attempts.last()?.workspace_id.as_deref()?;
        let question = run
            .question
            .as_ref()
            .filter(|question| question.generation == generation && question.answer.is_some())
            .and_then(|question| question.continuation.as_deref())
            .map(|id| (id, RunKind::DesignAnswer));
        let delegated = run
            .delegation
            .as_ref()
            .filter(|delegation| delegation.generation == generation && delegation.child.is_some())
            .and_then(|delegation| delegation.continuation.as_deref())
            .map(|id| (id, RunKind::DelegationResult));
        let (id, kind) = match (question, delegated) {
            (Some(edge), None) | (None, Some(edge)) => edge,
            _ => return None,
        };
        let next_index = *runs.get(id)?;
        let next = &history[next_index];
        let lineage = crate::delegation::lineage(run);
        if next.kind!=kind || next.continuation.is_none() || next.chat_id!=run.chat_id
            || next.actor!=run.actor || next.action!=run.action || next.backend!=run.backend || next.model!=run.model
            || next.context!=run.context || next.lenses!=run.lenses
            || next.persona.as_ref().is_some_and(|persona|persona!=&lineage)
            || (next.persona.is_none() && (run.persona.is_some() || kind==RunKind::DelegationResult))
            // A retried continuation has replaced its original attempt. It must
            // not revive a wake from a failed/cancelled execution generation.
            || next.attempts.first().and_then(|attempt|attempt.workspace_id.as_ref())!=next.attempts.last().and_then(|attempt|attempt.workspace_id.as_ref())
        {
            return None;
        }
        index = next_index;
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Disarm,
    Wait,
    Advance(usize),
    Queue(usize),
}

/// Plans are valid only for this exclusively borrowed snapshot. Do not retain
/// their positional indices across mutation or suspension. Authority, queue
/// capacity, durable writes and dispatcher effects remain the driver's job.
pub(crate) fn plan<'a>(
    view: View<'_>,
    watches: impl Iterator<Item = (usize, WakeRef<'a>)>,
    mut authorized: impl FnMut(&Actor, Action) -> bool,
) -> Vec<(usize, Decision)> {
    let runs: HashMap<_, _> = view
        .runs
        .iter()
        .enumerate()
        .map(|(i, run)| (run.id.as_str(), i))
        .collect();
    let busy: HashSet<_> = view
        .runs
        .iter()
        .filter(|run| {
            matches!(
                run.status.as_str(),
                "queued" | "running" | "cancelling" | "waiting"
            )
        })
        .filter_map(|run| run.chat_id.as_deref())
        .collect();
    let mut latest = HashMap::<&str, u64>::new();

    for run in view.runs.iter().filter(|run| normal_task(run)) {
        if let Some(chat) = run.chat_id.as_deref() {
            latest
                .entry(chat)
                .and_modify(|order| *order = (*order).max(run.queue_order))
                .or_insert(run.queue_order);
        }
    }
    watches
        .map(|(i, watch)| {
            let decision = (|| {
                let Some(&origin_index) = runs.get(watch.origin) else {
                    return Decision::Disarm;
                };
                let origin = &view.runs[origin_index];
                if !authorized(watch.actor, origin.action)
                    || origin.actor != *watch.actor
                    || origin.chat_id.as_deref() != Some(watch.chat)
                    || !view.chats.iter().any(|chat| {
                        chat.id == watch.chat
                            && chat.actor == *watch.actor
                            && chat.room.as_deref() == Some(watch.room)
                    })
                    || origin
                        .attempts
                        .last()
                        .and_then(|a| a.workspace_id.as_deref())
                        != Some(watch.generation)
                    || latest
                        .get(watch.chat)
                        .is_some_and(|order| *order > origin.queue_order)
                    || !matches!(
                        origin.status.as_str(),
                        "queued" | "running" | "waiting" | "completed" | "continued"
                    )
                    || watch.cursor > view.posts.len()
                {
                    return Decision::Disarm;
                }
                let Some(leaf_index) = continuation_leaf(view.runs, origin_index, &runs) else {
                    return Decision::Disarm;
                };
                let leaf = &view.runs[leaf_index];
                if !matches!(
                    leaf.status.as_str(),
                    "queued" | "running" | "waiting" | "completed"
                ) {
                    return Decision::Disarm;
                }
                if leaf.status != "completed" || busy.contains(watch.chat) {
                    return Decision::Wait;
                }
                let scanned = watch.cursor.max(watch.scanned);
                if scanned > view.posts.len() {
                    return Decision::Disarm;
                }
                let through = scanned.saturating_add(WINDOW).min(view.posts.len());
                if view.posts[scanned..through].iter().any(|post| {
                    post.room == watch.room
                        && post.chat.as_deref() != Some(watch.chat)
                        && post.delivery == crate::rooms::Delivery::Notify
                }) {
                    Decision::Queue(origin_index)
                } else if through > scanned {
                    Decision::Advance(through)
                } else {
                    Decision::Wait
                }
            })();
            (i, decision)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_admission_requires_authority_generation_and_actionable_peer_evidence() {
        let mut origin = super::super::runs::Submission {
            objective: "Work".into(),
            kind: RunKind::Chat,
            source: "test".into(),
            actor: Actor::LocalOperator,
            action: Action::Task,
        }
        .into_run("origin".into(), 1, Default::default(), None);
        origin.chat_id = Some("chat".into());
        origin.status = "completed".into();
        origin.attempts.push(crate::usage::Attempt {
            workspace_id: Some("attempt".into()),
            ..Default::default()
        });
        let runs = [origin];
        let chats = [serde_json::from_value::<Session>(serde_json::json!({
            "id":"chat", "actor":Actor::LocalOperator, "title":"Work",
            "messages":[], "room":"research"
        }))
        .unwrap()];
        let query = |allowed, generation, posts: &[Post]| {
            plan(
                View {
                    runs: &runs,
                    chats: &chats,
                    posts,
                },
                std::iter::once((
                    7,
                    WakeRef {
                        chat: "chat",
                        actor: &Actor::LocalOperator,
                        room: "research",
                        cursor: 0,
                        origin: "origin",
                        generation,
                        scanned: 0,
                    },
                )),
                |_, _| allowed,
            )
        };
        let mut posts = vec![
            Post {
                room: "research".into(),
                text: "Own output".into(),
                chat: Some("chat".into()),
                run: None,
                key: String::new(),
                delivery: crate::rooms::Delivery::Notify,
            },
            Post {
                room: "research".into(),
                text: "Recorded observation".into(),
                chat: Some("peer".into()),
                run: None,
                key: String::new(),
                delivery: crate::rooms::Delivery::Record,
            },
        ];
        assert_eq!(
            query(true, "attempt", &posts),
            vec![(7, Decision::Advance(2))]
        );
        posts[1].delivery = crate::rooms::Delivery::Notify;
        assert_eq!(
            query(true, "attempt", &posts),
            vec![(7, Decision::Queue(0))]
        );
        assert_eq!(query(false, "attempt", &posts), vec![(7, Decision::Disarm)]);
        assert_eq!(query(true, "stale", &posts), vec![(7, Decision::Disarm)]);
    }
}
