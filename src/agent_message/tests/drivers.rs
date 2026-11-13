//! Conformance fixtures, not a production async runtime. Tokio only interprets
//! fake archive/checkpoint effects; no credentials, model traffic or live state.
use super::*;
use operation::{Decision, Effect};
use std::{cell::RefCell, sync::Arc};

fn fixture(peer: bool) -> (State, String, String, &'static str) {
    let (mut state, lead, child, _) = tests::family();
    if peer {
        let target = tests::spawned(&mut state, &lead);
        let origin = tests::activate(&mut state, &child);
        (state, origin, target, "peer-attempt")
    } else {
        let origin = state.runtime.runs[0].id.clone();
        (state, origin, child, "attempt-a")
    }
}
fn call(target: &str) -> MessageCall {
    MessageCall {
        chat_id: target.into(),
        text: "A dependency changed; continue independently".into(),
        services: vec![],
    }
}
async fn asynchronous(
    state: &mut State,
    attempt: &AttemptRef<'_>,
    target: &str,
    failure: Option<Effect>,
    trace: &mut Vec<Effect>,
) -> io::Result<MessageResult> {
    let decision = prepare(state, attempt, call(target), "fixed-message-id".into())?;
    let delivery = match decision {
        Decision::Existing(result) => return Ok(result),
        Decision::Deliver(delivery) => delivery,
    };
    let mut flight = InFlight::new(state, *delivery);
    while let Some(effect) = flight.next() {
        // Actual suspension exercises the guard/borrows, not a future that is
        // immediately ready. The operation itself has no runtime dependency.
        tokio::task::yield_now().await;
        trace.push(effect);
        if failure == Some(effect) {
            return Err(fail("injected I/O failure"));
        }
        if effect == Effect::Checkpoint {
            let _durable_snapshot = serde_json::to_vec(&*flight.state).unwrap();
        }
        flight.completed(effect);
    }
    Ok(flight.result())
}
#[test]
fn blocking_and_tokio_drivers_share_effect_order_failure_rollback_and_restart_receipts() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    for peer in [false, true] {
        for failure in [
            None,
            Some(Effect::Checkpoint),
            Some(Effect::ArchiveEvidence),
        ]
        .into_iter()
        .filter(|failure| peer || *failure != Some(Effect::ArchiveEvidence))
        {
            let (mut blocking, origin, target, generation) = fixture(peer);
            let before = serde_json::to_vec(&blocking).unwrap();
            let mut asynchronous_state: State = serde_json::from_slice(&before).unwrap();
            let attempt = AttemptRef {
                run: &origin,
                generation,
                nonce: "same-request",
            };
            let trace = RefCell::new(Vec::new());
            let first = transact_at(
                &mut blocking,
                &attempt,
                call(&target),
                "fixed-message-id".into(),
                |_| {
                    trace.borrow_mut().push(Effect::ArchiveEvidence);
                    if failure == Some(Effect::ArchiveEvidence) {
                        Err(fail("injected I/O failure"))
                    } else {
                        Ok(())
                    }
                },
                |state| {
                    trace.borrow_mut().push(Effect::Checkpoint);
                    let _snapshot = serde_json::to_vec(state).unwrap();
                    if failure == Some(Effect::Checkpoint) {
                        Err(fail("injected I/O failure"))
                    } else {
                        Ok(())
                    }
                },
                || trace.borrow_mut().push(Effect::WakeDispatcher),
            );
            let mut other_trace = Vec::new();
            let second = runtime.block_on(asynchronous(
                &mut asynchronous_state,
                &attempt,
                &target,
                failure,
                &mut other_trace,
            ));
            assert_eq!(*trace.borrow(), other_trace);
            assert_eq!(
                first.as_ref().map_err(ToString::to_string),
                second.as_ref().map_err(ToString::to_string)
            );
            assert_eq!(
                serde_json::to_vec(&blocking).unwrap(),
                serde_json::to_vec(&asynchronous_state).unwrap()
            );
            if failure.is_some() {
                assert_eq!(serde_json::to_vec(&blocking).unwrap(), before);
                assert!(!other_trace.contains(&Effect::WakeDispatcher));
            } else {
                let expected = if peer {
                    vec![
                        Effect::ArchiveEvidence,
                        Effect::Checkpoint,
                        Effect::WakeDispatcher,
                    ]
                } else {
                    vec![Effect::Checkpoint, Effect::WakeDispatcher]
                };
                assert_eq!(other_trace, expected);
                let mut restored: State =
                    serde_json::from_slice(&serde_json::to_vec(&blocking).unwrap()).unwrap();
                let replay = transact_at(
                    &mut restored,
                    &attempt,
                    call(&target),
                    "unused-id".into(),
                    |_| panic!("receipt retry must not archive"),
                    |_| panic!("receipt retry must not checkpoint"),
                    || panic!("receipt retry must not wake"),
                )
                .unwrap();
                assert_eq!(replay, first.unwrap());
            }
        }
    }
}
#[test]
fn dropping_a_suspended_driver_rolls_back_before_a_waiting_writer_can_cancel_the_origin() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (state, origin, target, generation) = fixture(true);
        let before = serde_json::to_vec(&state).unwrap();
        let shared = Arc::new(tokio::sync::Mutex::new(state));
        let (started, ready) = tokio::sync::oneshot::channel();
        let pending_state = shared.clone();
        let pending_origin = origin.clone();
        let delivery = tokio::spawn(async move {
            let mut state = pending_state.lock().await;
            let decision = prepare(
                &state,
                &AttemptRef {
                    run: &pending_origin,
                    generation,
                    nonce: "cancelled-driver",
                },
                call(&target),
                "fixed-id".into(),
            )
            .unwrap();
            let Decision::Deliver(delivery) = decision else {
                panic!("fresh fixture")
            };
            let mut flight = InFlight::new(&mut state, *delivery);
            // The fake archive succeeded; suspend at the persistence boundary.
            flight.completed(Effect::ArchiveEvidence);
            started.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        ready.await.unwrap();
        assert!(
            shared.try_lock().is_err(),
            "tentative state is not exposed to another writer"
        );
        let writer_state = shared.clone();
        let cancelled_origin = origin.clone();
        let writer = tokio::spawn(async move {
            let mut state = writer_state.lock().await;
            assert_eq!(serde_json::to_vec(&*state).unwrap(), before);
            state
                .runtime
                .runs
                .iter_mut()
                .find(|run| run.id == cancelled_origin)
                .unwrap()
                .status = "cancelled".into();
        });
        tokio::task::yield_now().await;
        assert!(
            !writer.is_finished(),
            "the next writer waits for the unfinished transaction"
        );
        delivery.abort();
        assert!(delivery.await.unwrap_err().is_cancelled());
        writer.await.unwrap();
        let state = shared.lock().await;
        let decision = prepare(
            &state,
            &AttemptRef {
                run: &origin,
                generation,
                nonce: "after-cancel",
            },
            call(&state.chats[1].id),
            "another-id".into(),
        );
        assert!(
            decision.is_err(),
            "cancelled attempts cannot create new effects"
        );
    });
}
#[test]
fn dropping_after_a_confirmed_checkpoint_preserves_committed_work_and_its_receipt() {
    let (mut state, origin, target, generation) = fixture(true);
    let attempt = AttemptRef {
        run: &origin,
        generation,
        nonce: "committed",
    };
    let decision = prepare(&state, &attempt, call(&target), "committed-id".into()).unwrap();
    let Decision::Deliver(delivery) = decision else {
        panic!("fresh fixture")
    };
    let mut flight = InFlight::new(&mut state, *delivery);
    assert_eq!(flight.next(), Some(Effect::ArchiveEvidence));
    flight.completed(Effect::ArchiveEvidence);
    flight.completed(Effect::Checkpoint);
    assert_eq!(flight.next(), Some(Effect::WakeDispatcher));
    drop(flight);
    let Decision::Existing(result) =
        prepare(&state, &attempt, call(&target), "unused-id".into()).unwrap()
    else {
        panic!("committed receipt must survive interpreter cancellation")
    };
    assert_eq!(result.run_id, "committed-id");
}
#[test]
fn planning_is_read_only_and_deterministic_with_explicit_ids_and_rejects_stale_attempts() {
    let (mut state, origin, target, generation) = fixture(true);
    let before = serde_json::to_vec(&state).unwrap();
    let attempt = AttemptRef {
        run: &origin,
        generation,
        nonce: "pure-plan",
    };
    let plan = || {
        operation::plan(
            operation::View {
                runs: &state.runtime.runs,
                chats: &state.chats,
                services: &state.services,
                sent: &state.agent_messages,
                bots: &state.bots,
            },
            &attempt,
            call(&target),
            "explicit-id".into(),
        )
        .unwrap()
    };
    let (Decision::Deliver(first), Decision::Deliver(second)) = (plan(), plan()) else {
        panic!("fresh fixture")
    };
    assert_eq!(
        serde_json::to_vec(&first.run).unwrap(),
        serde_json::to_vec(&second.run).unwrap()
    );
    assert_eq!(first.result, second.result);
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    state
        .runtime
        .runs
        .iter_mut()
        .find(|run| run.id == origin)
        .unwrap()
        .attempts
        .last_mut()
        .unwrap()
        .workspace_id = Some("new-generation".into());
    assert!(prepare(&state, &attempt, call(&target), "new-id".into()).is_err());
}
#[test]
fn transport_encodes_the_typed_result_and_can_use_a_host_without_global_state() {
    struct MemoryHost;
    impl MessageOps for MemoryHost {
        fn message(
            &self,
            attempt: &AttemptRef<'_>,
            call: MessageCall,
        ) -> io::Result<MessageResult> {
            assert_eq!(
                (attempt.run, attempt.generation, attempt.nonce),
                ("run", "generation", "request")
            );
            assert_eq!(
                (call.chat_id.as_str(), call.text.as_str()),
                ("peer", "hello")
            );
            Ok(MessageResult {
                chat_id: call.chat_id,
                run_id: "accepted".into(),
                queued: false,
                services: call.services,
                retained: Some(true),
            })
        }
    }
    let attempt = AttemptRef {
        run: "run",
        generation: "generation",
        nonce: "request",
    };
    assert_eq!(
        run(
            &MemoryHost,
            &attempt,
            &json!({"chat_id":"peer","text":"hello"})
        )
        .unwrap(),
        json!({"chat_id":"peer","run_id":"accepted","queued":false,"services":[],"retained":true})
    );
    assert!(
        run(
            &MemoryHost,
            &attempt,
            &json!({"chat_id":"peer","text":"hello","unknown":true})
        )
        .is_err()
    );
}
