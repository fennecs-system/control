
use super::*;
use std::sync::Mutex;

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);
impl WatchOps for Recorder {
    fn watch(&self, _: &AttemptRef<'_>, call: WatchCall) -> io::Result<Value> {
        let call = match call {
            WatchCall::Start {
                name,
                permanent,
                wake,
                ..
            } => format!("start {name} {permanent} {wake}"),
            WatchCall::Events {
                watch_id, after, ..
            } => format!("events {watch_id} {after:?}"),
            WatchCall::Status { watch_id } => format!("status {watch_id:?}"),
            WatchCall::Stop { watch_id } => format!("stop {watch_id}"),
            WatchCall::Wake => "wake".into(),
        };
        self.0.lock().unwrap().push(call);
        Ok(json!({}))
    }
}

#[test]
fn watch_tools_decode_typed_calls_against_only_their_capability() {
    let host = Recorder::default();
    let attempt = AttemptRef {
        run: "r",
        generation: "g",
        nonce: "n",
    };
    for (name, args) in [
        (
            "control_watch_start",
            json!({"service_id":"svc","name":"chat","command":"node bot.js"}),
        ),
        (
            "control_watch_start",
            json!({"service_id":"svc","name":"once","command":"ls","restart":"transient"}),
        ),
        ("control_watch_events", json!({"watch_id":"w-1","after":4})),
        ("control_watch_status", json!({})),
        ("control_watch_stop", json!({"watch_id":"w-1"})),
        (
            "control_watch_start",
            json!({"service_id":"svc","name":"ready","command":"tail -F log","wake":true}),
        ),
        ("control_watch_wake", json!({})),
        (
            "control_watch_start",
            json!({"service_id":"svc","name":"quiet","command":"ls","wake":false}),
        ),
    ] {
        run(&host, &attempt, name, &args).unwrap();
    }
    assert_eq!(
        *host.0.lock().unwrap(),
        [
            "start chat true true",
            "start once false true",
            "events w-1 Some(4)",
            "status None",
            "stop w-1",
            "start ready true true",
            "wake",
            "start quiet true false"
        ]
    );
    assert!(
        run(
            &host,
            &attempt,
            "control_watch_start",
            &json!({"service_id":"svc","name":"x","command":"ls","restart":"sometimes"})
        )
        .is_err()
    );
    assert!(
        run(
            &host,
            &attempt,
            "control_watch_start",
            &json!({"service_id":"svc","name":" ","command":"ls"})
        )
        .is_err()
    );
    assert!(
        run(
            &host,
            &attempt,
            "control_watch_stop",
            &json!({"watch_id":"w-1","extra":1})
        )
        .is_err()
    );
    assert!(
        run(
            &host,
            &attempt,
            "control_watch_wake",
            &json!({"watch_id":"w-1"})
        )
        .is_err()
    );
    let call = decode("control_watch_start", &json!({"service_id":"svc","name":"filtered","command":"tail -F log","wake_policy":{"kind":"prefixes","prefixes":["event:"]}})).unwrap();
    assert!(matches!(
        call,
        WatchCall::Start {
            wake_policy: WakePolicy::Prefixes { .. },
            ..
        }
    ));
    assert!(decode("control_watch_start", &json!({"service_id":"svc","name":"filtered","command":"tail -F log","wake_policy":{"kind":"prefixes","prefixes":[]}})).is_err());
    assert!(matches!(
        decode(
            "control_watch_start",
            &json!({"service_id":"svc","name":"task","command":"worker","notifications":true})
        )
        .unwrap(),
        WatchCall::Start {
            notifications: true,
            wake_policy: WakePolicy::ExitOnly {},
            ..
        }
    ));
    assert!(decode("control_watch_start",&json!({"service_id":"svc","name":"task","command":"worker","notifications":true,"wake_policy":{"kind":"any_line"}})).is_err());
}
