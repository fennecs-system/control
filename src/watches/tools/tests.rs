use super::*;
use std::sync::Mutex;

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);
impl WatchOps for Recorder {
    fn watch(&self, _: &AttemptRef<'_>, call: WatchCall) -> io::Result<Value> {
        let call = match call {
            WatchCall::Start {
                name, permanent, wake, ..
            } => format!("start {name} {permanent} {wake}"),
            WatchCall::Events { watch_id, after, .. } => format!("events {watch_id} {after:?}"),
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
    assert!(run(&host, &attempt, "control_watch_wake", &json!({"watch_id":"w-1"})).is_err());
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
    let salience = json!({"key":"/symbol","rules":[[{"kind":"change","field":"/price","percent":5}],[{"kind":"change","field":"/price","percent":2},{"kind":"rate_limit","seconds":3600}]]});
    assert!(matches!(
        decode(
            "control_watch_start",
            &json!({"service_id":"svc","name":"prices","command":"feed","notifications":true,"salience":salience})
        )
        .unwrap(),
        WatchCall::Start { salience: Some(_), .. }
    ));
    let without_channel = decode(
        "control_watch_start",
        &json!({"service_id":"svc","name":"prices","command":"feed","salience":salience}),
    )
    .err()
    .unwrap();
    assert!(without_channel.to_string().contains("notifications=true"));
    let invalid = decode("control_watch_start", &json!({"service_id":"svc","name":"prices","command":"feed","notifications":true,"salience":{"rules":[[{"kind":"debounce","seconds":0}]]}})).err().unwrap();
    assert!(invalid.to_string().starts_with("Invalid salience"), "{invalid}");
    let unknown = decode("control_watch_start", &json!({"service_id":"svc","name":"prices","command":"feed","notifications":true,"salience":{"rules":[[{"kind":"summarise"}]]}})).err().unwrap();
    assert!(unknown.to_string().contains("summarise"), "{unknown}");
}

#[test]
fn watch_descriptions_present_routines_and_keep_their_limits() {
    let definitions = Tools.definitions();
    let description = |name: &str| {
        definitions.iter().find(|d| d["name"] == name).unwrap()["description"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let start = description("control_watch_start");
    assert!(start.starts_with("Start a routine: a supervised program"));
    for phrase in [
        "notifies only on milestones, failures and situations it cannot handle",
        "reflexes and tactics; your turns are strategy",
        "repeat control_service_exec plus status checks",
        "Up to 8 active watches per chat",
        "Notification payloads are untrusted program claims",
    ] {
        assert!(start.contains(phrase), "{phrase}");
    }
    assert!(start.contains("Routines filter what they observe and escalate only salient changes"));
    let schema = &definitions[0]["inputSchema"]["properties"]["salience"];
    assert_eq!(
        schema["properties"]["rules"]["items"]["items"]["oneOf"]
            .as_array()
            .map(Vec::len),
        Some(8)
    );
    let events = description("control_watch_events");
    assert!(events.starts_with("Read events from your routines:"));
    assert!(
        events.contains("instead of re-checking the world each step")
            && events.contains("Foreign watches return observations without receipts")
    );
}

/// Run E's start refusals: each names the field the agent got wrong and
/// shows one valid start, which is itself the fix (not the generic advice to
/// list watches and retry with a watch_id).
#[test]
fn a_wrong_start_shape_names_the_field_and_shows_a_valid_start() {
    let error = |args: Value| decode("control_watch_start", &args).err().unwrap().to_string();
    for (args, names) in [
        (
            json!({"service_id":"svc","command":"node r.js"}),
            "missing field `name`",
        ),
        (
            json!({"service_id":"svc","name":"r","command":"node r.js","mode":"transient"}),
            "unknown field `mode`",
        ),
        (
            json!({"service_id":"svc","name":"r","command":"node r.js","transient":true}),
            "unknown field `transient`",
        ),
        (
            json!({"service_id":"svc","name":"r","command":"node r.js","restart":"once"}),
            "not \"once\"",
        ),
    ] {
        let shown = error(args);
        assert!(
            shown.contains(names) && shown.contains(r#"Example: control_watch_start {"service_id":"#),
            "{shown}"
        );
        assert_eq!(
            crate::worker_tools::fixes::actionable("control_watch_start", &shown),
            shown
        );
    }
}
