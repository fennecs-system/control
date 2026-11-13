
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
    let state = mailbox.publication(None, "r", "g", "cp").unwrap().unwrap();
    assert_eq!(state.processed[0].through(), 0);
    let mut unseen = Mailbox::default();
    unseen.deliver("empty".into(), page(10, 10)).unwrap();
    unseen
        .stage("draft".into(), handoff(&["empty"]), 0)
        .unwrap();
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
