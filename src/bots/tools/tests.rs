
use super::*;
use std::sync::Mutex;

/// Provides only bot operations: tool families need no other capability.
#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);
impl BotOps for Recorder {
    fn bot(&self, attempt: &AttemptRef<'_>, call: BotCall) -> io::Result<Value> {
        let call = match call {
            BotCall::Pause => "pause".into(),
            BotCall::Request {
                chat_id,
                every_minutes,
                room,
            } => format!("request {chat_id} {every_minutes:?} {room}"),
            BotCall::Status { chat_id } => format!("status {chat_id:?}"),
        };
        self.0
            .lock()
            .unwrap()
            .push(format!("{}/{} {call}", attempt.run, attempt.nonce));
        Ok(json!({"recorded":true}))
    }
}

#[test]
fn bot_tools_decode_typed_calls_against_a_host_offering_only_bot_operations() {
    let host = Recorder::default();
    let attempt = AttemptRef {
        run: "run",
        generation: "gen",
        nonce: "n1",
    };
    run(
        &host,
        &attempt,
        "control_bot_request",
        &json!({"chat_id":"c-1","every_minutes":5}),
    )
    .unwrap();
    run(&host, &attempt, "control_bot_status", &json!({})).unwrap();
    run(&host, &attempt, "control_bot_pause", &json!({})).unwrap();
    assert!(
        run(
            &host,
            &attempt,
            "control_bot_pause",
            &json!({"chat_id":"c-other"})
        )
        .is_err()
    );
    assert!(
        run(
            &host,
            &attempt,
            "control_bot_request",
            &json!({"chat_id":"c-1","every_minutes":5,"extra":1})
        )
        .is_err()
    );
    assert!(run(&host, &attempt, "control_bot_spawn", &json!({})).is_err());
    assert_eq!(
        *host.0.lock().unwrap(),
        [
            "run/n1 request c-1 Some(5) true",
            "run/n1 status None",
            "run/n1 pause"
        ]
    );
}
