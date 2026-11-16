
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
    mailbox
        .stage("draft".into(), handoff(&["empty"]), 0)
        .unwrap();
    let state = mailbox.publication(None, "r", "g", "cp", |_| true).unwrap().unwrap();
    assert_eq!(state.processed[0].through(), 0);
    let mut unseen = Mailbox::default();
    unseen.deliver("empty".into(), page(10, 10)).unwrap();
    unseen
        .stage("draft".into(), handoff(&["empty"]), 0)
        .unwrap();
    assert!(unseen.publication(None, "r", "g", "cp", |_| true).is_err());
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
    let previous = first.publication(None, "r1", "g1", "cp1", |_| true).unwrap().unwrap();
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
        .publication(Some(&previous), "r2", "g2", "cp2", |_| true)
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
            .publication(None, "run", "attempt", "checkpoint", |_| true)
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
        .publication(None, "run", "attempt", "checkpoint", |_| true)
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
            .publication(None, "run", "attempt", "checkpoint", |_| true)
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
            .publication(None, "run", "attempt", "checkpoint", |_| true)
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
fn watch(id: &str, after: u64, through: u64) -> Range {
    Range::Watch {
        watch: id.into(),
        after,
        through,
    }
}
fn watch_page(range: Range) -> Delivery {
    Delivery {
        ranges: vec![range],
        gap: false,
        request: String::new(),
    }
}
/// Committed state at the source budget: finished task watches plus one live.
fn at_budget() -> WorkingState {
    let mut processed = (0..MAX_SOURCES - 1)
        .map(|n| watch(&format!("w-done-{n}"), 0, 4))
        .collect::<Vec<_>>();
    processed.push(watch("w-live", 0, 5));
    WorkingState {
        revision: 3,
        run: "r".into(),
        generation: "g".into(),
        checkpoint: "cp".into(),
        payload: handoff(&[]).payload,
        processed,
        disposition: Disposition::Continue,
    }
}
fn later(processed: &[&str]) -> Handoff {
    Handoff {
        base_revision: 3,
        ..handoff(processed)
    }
}
fn retired(range: &Range) -> bool {
    !matches!(range, Range::Watch { watch, .. } if watch.starts_with("w-done"))
}
#[test]
fn retired_sources_no_longer_exhaust_the_budget() {
    let previous = at_budget();
    let mut mailbox = Mailbox::default();
    mailbox.baseline.push(watch("w-next", 0, 0));
    mailbox.stage("h".into(), later(&[]), 3).unwrap();
    assert!(
        mailbox
            .publication(Some(&previous), "r", "g", "cp", |_| true)
            .is_err()
    );
    let state = mailbox
        .publication(Some(&previous), "r", "g", "cp", retired)
        .unwrap()
        .unwrap();
    assert_eq!(state.revision, 4);
    assert_eq!(
        state.processed,
        vec![watch("w-live", 0, 5), watch("w-next", 0, 0)]
    );
}
#[test]
fn a_live_watch_keeps_its_watermark_and_cannot_skip_unseen_events() {
    let previous = at_budget();
    let mut mailbox = Mailbox::default();
    mailbox
        .deliver("next".into(), watch_page(watch("w-live", 5, 9)))
        .unwrap();
    mailbox.stage("h".into(), later(&["next"]), 3).unwrap();
    let state = mailbox
        .publication(Some(&previous), "r", "g", "cp", retired)
        .unwrap()
        .unwrap();
    assert_eq!(state.processed, vec![watch("w-live", 0, 9)]);
    // Retiring other sources never relaxes contiguity for a live one.
    let mut skipping = Mailbox::default();
    skipping
        .deliver("skip".into(), watch_page(watch("w-live", 7, 9)))
        .unwrap();
    skipping.stage("h".into(), later(&["skip"]), 3).unwrap();
    assert!(
        skipping
            .publication(Some(&previous), "r", "g", "cp", retired)
            .is_err()
    );
}
#[test]
fn a_receipt_for_a_source_retired_mid_attempt_still_publishes() {
    // The receipt is checked against the previous watermark before the
    // retired source is dropped, so processed evidence is never rejected.
    let previous = at_budget();
    let mut mailbox = Mailbox::default();
    mailbox
        .deliver("last".into(), watch_page(watch("w-done-0", 4, 6)))
        .unwrap();
    mailbox.stage("h".into(), later(&["last"]), 3).unwrap();
    let state = mailbox
        .publication(Some(&previous), "r", "g", "cp", retired)
        .unwrap()
        .unwrap();
    assert_eq!(state.processed, vec![watch("w-live", 0, 5)]);
}
#[test]
fn applicable_receipts_keep_contiguous_ranges_and_explain_the_rest() {
    let mut mailbox = Mailbox::default();
    mailbox.deliver("a".into(), page(0, 2)).unwrap();
    mailbox.deliver("b".into(), page(2, 4)).unwrap();
    mailbox.deliver("late".into(), page(6, 8)).unwrap();
    let gap = Delivery { gap: true, ..page(4, 6) };
    mailbox.deliver("gap".into(), gap).unwrap();
    let all: Vec<String> = ["b", "a", "a", "late", "gap", "unknown"].map(String::from).into();
    let (kept, dropped) = mailbox.applicable(None, &all, &[]).unwrap();
    assert_eq!(kept, ["b", "a"]);
    let reasons: Vec<_> = dropped.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(reasons, ["gap", "unknown", "late"]);
    // The kept receipts publish; the dropped ones stay pending.
    mailbox.stage("h".into(), handoff(&["b", "a"]), 0).unwrap();
    let state = mailbox.publication(None, "r", "g", "cp", |_| true).unwrap().unwrap();
    assert_eq!(state.processed[0].through(), 4);
    // Resynchronising the gap admits it, and then the later page follows.
    let (kept, dropped) = mailbox.applicable(None, &all, &["gap".into()]).unwrap();
    assert_eq!(kept, ["b", "a", "late", "gap"]);
    assert_eq!(dropped.len(), 1);
    // Only the accepted draft survives a full correction budget.
    for index in 0..12 {
        mailbox.stage(format!("c{index}"), handoff(&[]), 0).unwrap();
    }
    assert!(mailbox.drafts.len() <= 8);
    assert_eq!(mailbox.accepted.as_deref(), Some("c11"));
}
